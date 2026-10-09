use crate::{
    auth,
    core::{error, AppState},
    db,
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::Row;
use std::time::Duration;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/status", get(status))
        .route(
            "/api/plans",
            get(|| async { Json(crate::licensing::catalog()) }),
        )
        .route(
            "/api/setup",
            post(auth::setup).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/login",
            post(auth::login).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/api/logout", post(auth::logout))
        .route("/api/auth/oidc/start", get(crate::sso::start))
        .route("/api/auth/oidc/callback", get(crate::sso::callback))
        .route("/api/dashboard", get(dashboard))
        .route("/api/license", get(license_status))
        .route("/api/updates", get(crate::updates::api))
        .route(
            "/api/updates/policy",
            axum::routing::post(crate::updates::set_policy),
        )
        .route(
            "/api/updates/apply",
            axum::routing::post(crate::updates::apply),
        )
        .route(
            "/api/connections",
            get(list_connections).post(create_connection),
        )
        .route("/api/providers", get(list_providers).post(create_provider))
        .route(
            "/api/notification-providers",
            get(list_notification_providers).post(create_notification_provider),
        )
        .route(
            "/api/automations",
            get(list_automations).post(create_automation),
        )
        .route("/api/runs", get(list_runs))
        .route(
            "/api/connections/{id}",
            put(update_connection).delete(delete_connection),
        )
        .route(
            "/api/providers/{id}",
            put(update_provider).delete(delete_provider),
        )
        .route(
            "/api/notification-providers/{id}",
            put(update_notification_provider).delete(delete_notification_provider),
        )
        .route(
            "/api/automations/{id}",
            put(update_automation).delete(delete_automation),
        )
        .route("/api/connections/{id}/test", post(test_connection))
        .route("/api/providers/{id}/test", post(test_provider))
        .route(
            "/api/notification-providers/{id}/test",
            post(test_notification_provider),
        )
        .route("/api/automations/{id}/run", post(run_automation))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/runs/{id}/cancel", post(cancel_run))
        .route("/api/runs/{id}/retry", post(retry_run))
        .route("/api/schedule/preview", post(schedule_preview))
        .route("/api/webhooks/{connection_id}", post(webhook))
        .route("/api/{*path}", axum::routing::any(api_not_found))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::security::protect,
        ))
        .with_state(state)
}
async fn health(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(1) => Json(json!({"ok":true})).into_response(),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "Database unavailable"),
    }
}
async fn api_not_found() -> Response {
    error(StatusCode::NOT_FOUND, "API route not found")
}

// Axum handlers return the response directly; boxing here would add an allocation per rejection.
#[allow(clippy::result_large_err)]
pub(crate) async fn guard(state: &AppState, h: &HeaderMap, mutating: bool) -> Result<(), Response> {
    match auth::authenticate(state, h).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(error(StatusCode::UNAUTHORIZED, "Authentication required")),
        Err(e) => return Err(error(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
    if mutating && !state.security.origin_ok(h) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Invalid request origin or CSRF header",
        ));
    }
    Ok(())
}
async fn status(State(s): State<AppState>, h: HeaderMap) -> Response {
    let setup = s.security.local_login && auth::is_setup_required(&s).await.unwrap_or(true);
    let sso = s.security.oidc.is_some() && s.license.can_start_sso(&s.db).await.unwrap_or(false);
    Json(json!({"setup_required":setup,"authenticated":auth::authenticate(&s,&h).await.ok().flatten().is_some(),"version":env!("CARGO_PKG_VERSION"),"sso_enabled":sso,"sso_requires_license":s.security.oidc.is_some() && !sso,"installation_id":s.license.installation_id,"local_login_enabled":s.security.local_login})).into_response()
}
async fn license_status(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(r) = guard(&s, &h, false).await {
        return r;
    }
    match s.license.status(&s.db).await {
        Ok(status) => Json(status).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
async fn list_objects(s: &AppState, h: &HeaderMap, kind: &str) -> Response {
    if let Err(r) = guard(s, h, false).await {
        return r;
    }
    match db::objects(&s.db, kind).await {
        Ok(mut rows) => {
            for row in &mut rows {
                if let Err(e) = s.mask(kind, row) {
                    return error(StatusCode::INTERNAL_SERVER_ERROR, e);
                }
            }
            Json(rows).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
async fn list_connections(State(s): State<AppState>, h: HeaderMap) -> Response {
    list_objects(&s, &h, "connections").await
}
async fn list_providers(State(s): State<AppState>, h: HeaderMap) -> Response {
    list_objects(&s, &h, "providers").await
}
async fn list_notification_providers(State(s): State<AppState>, h: HeaderMap) -> Response {
    list_objects(&s, &h, "notification_providers").await
}
async fn list_automations(State(s): State<AppState>, h: HeaderMap) -> Response {
    list_objects(&s, &h, "automations").await
}

fn validate(kind: &str, v: &Value) -> Result<(), String> {
    if !v.is_object() {
        return Err("Expected a JSON object".into());
    }
    let strf = |k: &str| v[k].as_str().unwrap_or("");
    match kind {
        "connections" => {
            if strf("name").trim().is_empty() || !["github", "forgejo"].contains(&strf("kind")) {
                return Err("Connection name and supported kind are required".into());
            }
            let base = if strf("base_url").is_empty() {
                if strf("kind") == "github" {
                    "https://api.github.com"
                } else {
                    ""
                }
            } else {
                strf("base_url")
            };
            if !base.is_empty() {
                let u = url::Url::parse(base).map_err(|_| "Invalid connection URL")?;
                if !["http", "https"].contains(&u.scheme())
                    || u.username() != ""
                    || u.password().is_some()
                {
                    return Err(
                        "Connection URL must be HTTP(S) without embedded credentials".into(),
                    );
                }
            } else {
                return Err("Forgejo instance URL is required".into());
            }
        }
        "providers" => {
            if strf("name").trim().is_empty()
                || !["openai", "ollama", "anthropic"].contains(&strf("kind"))
            {
                return Err("Provider name and supported kind are required".into());
            }
            if strf("default_model").trim().is_empty() {
                return Err("Default model is required".into());
            }
            if strf("kind") == "openai" && strf("base_url").is_empty() {
                return Err("OpenAI-compatible API base URL is required".into());
            }
            if !strf("token_parameter").is_empty()
                && !["max_tokens", "max_completion_tokens"].contains(&strf("token_parameter"))
            {
                return Err("Unsupported token parameter".into());
            }
            if !strf("base_url").is_empty() {
                let u = url::Url::parse(strf("base_url")).map_err(|_| "Invalid provider URL")?;
                if !["http", "https"].contains(&u.scheme())
                    || u.username() != ""
                    || u.password().is_some()
                {
                    return Err("Provider URL must be HTTP(S) without embedded credentials".into());
                }
            }
        }
        "notification_providers" => {
            if strf("name").trim().is_empty()
                || !["discord", "slack", "teams", "webhook"].contains(&strf("kind"))
            {
                return Err("Notification provider name and supported type are required".into());
            }
            let raw = strf("url");
            if !raw.starts_with("enc:") {
                let u = url::Url::parse(raw).map_err(|_| "Notification URL is required")?;
                if !["http", "https"].contains(&u.scheme())
                    || !u.username().is_empty()
                    || u.password().is_some()
                {
                    return Err(
                        "Notification URL must use HTTP(S) without embedded credentials".into(),
                    );
                }
            }
        }
        "automations" => {
            if strf("name").trim().is_empty()
                || !["review", "fix_pr", "solve_issue", "audit"].contains(&strf("action"))
            {
                return Err("Automation name and supported action are required".into());
            }
            for field in ["trigger", "filters", "limits", "fix"] {
                if !v[field].is_object() {
                    return Err(format!("{field} must be an object"));
                }
            }
            let mode = v["fix"]["mode"].as_str().unwrap_or("new_branch");
            if !["new_branch", "existing_branch"].contains(&mode)
                || (strf("action") == "solve_issue" && mode == "existing_branch")
            {
                return Err(
                    "Issue fixes require a new branch; existing branch mode is only for PR fixes"
                        .into(),
                );
            }
            let destinations = v["notifications"]
                .as_array()
                .ok_or("notifications must be an array")?;
            for n in destinations {
                let kind = n["kind"].as_str().unwrap_or("");
                if !n.is_object()
                    || ![
                        "pr_comment",
                        "issue_comment",
                        "discord",
                        "slack",
                        "teams",
                        "webhook",
                    ]
                    .contains(&kind)
                {
                    return Err("Unsupported notification destination".into());
                }
                if !["pr_comment", "issue_comment"].contains(&kind)
                    && n["provider_id"].as_str().is_none_or(str::is_empty)
                {
                    let raw = n["url"].as_str().unwrap_or("");
                    if !raw.starts_with("enc:") {
                        let u = url::Url::parse(raw).map_err(|_| "Notification URL is required")?;
                        if !["http", "https"].contains(&u.scheme())
                            || !u.username().is_empty()
                            || u.password().is_some()
                        {
                            return Err(
                                "Notification URL must use HTTP(S) without embedded credentials"
                                    .into(),
                            );
                        }
                    }
                }
            }
            for k in ["connection_id", "provider_id"] {
                if strf(k).is_empty() {
                    return Err(format!("{k} is required"));
                }
            }
            if v["repositories"]
                .as_array()
                .is_none_or(|a| a.is_empty() || a.len() > 100)
            {
                return Err("Provide between 1 and 100 repositories".into());
            }
            for r in v["repositories"].as_array().unwrap() {
                let Some(r) = r.as_str() else {
                    return Err("Repository entries must be strings".into());
                };
                let parts: Vec<_> = r.split('/').collect();
                if parts.len() != 2
                    || parts
                        .iter()
                        .any(|p| p.is_empty() || p.contains([' ', ':', '\\']))
                {
                    return Err(format!("Invalid repository: {r}"));
                }
            }
            let l = &v["limits"];
            for (field, min, max) in [
                ("max_files", 1, 2000),
                ("max_file_bytes", 1024, 2_000_000),
                ("max_context_chars", 1000, 2_000_000),
                ("max_output_tokens", 100, 100_000),
                ("timeout_seconds", 5, 3600),
                ("max_fix_files", 1, 100),
            ] {
                let n = l[field].as_i64().unwrap_or(-1);
                if n < min || n > max {
                    return Err(format!("limits.{field} must be between {min} and {max}"));
                }
            }
            if v["trigger"]["schedule_enabled"] == true {
                let c = v["trigger"]["cron"].as_str().unwrap_or("");
                let normalized = normalize_cron(c).map_err(|_| "Invalid cron expression")?;
                cron::Schedule::from_str(&normalized).map_err(|_| "Invalid cron expression")?;
                let tz = v["trigger"]["timezone"].as_str().unwrap_or("UTC");
                tz.parse::<chrono_tz::Tz>()
                    .map_err(|_| "Invalid timezone")?;
            }
        }
        _ => return Err("Unknown object kind".into()),
    }
    Ok(())
}
use std::str::FromStr;
pub(crate) fn normalize_cron(expression: &str) -> Result<String, ()> {
    let fields: Vec<_> = expression.split_whitespace().collect();
    match fields.len() {
        5 => {
            let mut f = fields.iter().map(|x| x.to_string()).collect::<Vec<_>>();
            f[4] = normalize_weekdays(&f[4])?;
            Ok(format!("0 {}", f.join(" ")))
        }
        6 | 7 => Ok(expression.to_owned()),
        _ => Err(()),
    }
}
fn normalize_weekdays(field: &str) -> Result<String, ()> {
    let mut out = Vec::new();
    for part in field.split(',') {
        let (base, step) = part
            .split_once('/')
            .map_or((part, None), |(a, b)| (a, Some(b)));
        let (lo, hi) = if base == "*" {
            (0, 6)
        } else if let Some((lo, hi)) = base.split_once('-') {
            (day_number(lo)?, day_number(hi)?)
        } else {
            let start = day_number(base)?;
            (start, if step.is_some() { 7 } else { start })
        };
        let step = step
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| ())?
            .unwrap_or(1);
        if step == 0 || lo > hi {
            return Err(());
        }
        for day in (lo..=hi).step_by(step) {
            out.push(day_name(&day.to_string())?);
        }
    }
    Ok(out.join(","))
}
fn day_number(value: &str) -> Result<u8, ()> {
    let upper = value.to_ascii_uppercase();
    if let Some(day) = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]
        .iter()
        .position(|name| *name == upper)
    {
        return Ok(day as u8);
    }
    let day = value.parse::<u8>().map_err(|_| ())?;
    if day > 7 {
        return Err(());
    }
    Ok(day)
}
fn day_name(value: &str) -> Result<String, ()> {
    let n = value.parse::<u8>().map_err(|_| ())?;
    Ok(match n {
        0 | 7 => "SUN",
        1 => "MON",
        2 => "TUE",
        3 => "WED",
        4 => "THU",
        5 => "FRI",
        6 => "SAT",
        _ => return Err(()),
    }
    .to_owned())
}
async fn object_write(
    s: AppState,
    h: HeaderMap,
    kind: &'static str,
    id: Option<String>,
    Json(mut v): Json<Value>,
) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    if !v.is_object() {
        return error(StatusCode::BAD_REQUEST, "Expected a JSON object");
    }
    let is_create = id.is_none();
    let object_id = id.clone();
    if let Some(id) = id {
        if let Some(prev) = db::object(&s.db, kind, &id).await.ok().flatten() {
            let secrets = match kind {
                "connections" => vec!["token", "webhook_secret"],
                "providers" => vec!["api_key"],
                "notification_providers" => vec!["url"],
                _ => vec![],
            };
            for f in secrets {
                if v[f].as_str().unwrap_or("").is_empty() {
                    v[f] = prev[f].clone()
                }
            }
            if kind == "automations" {
                if let (Some(new), Some(old)) = (
                    v["notifications"].as_array_mut(),
                    prev["notifications"].as_array(),
                ) {
                    let mut new_counts = std::collections::HashMap::<String, usize>::new();
                    for n in new.iter() {
                        *new_counts
                            .entry(n["kind"].as_str().unwrap_or("").to_owned())
                            .or_default() += 1;
                    }
                    let mut old_counts = std::collections::HashMap::<String, usize>::new();
                    for o in old {
                        *old_counts
                            .entry(o["kind"].as_str().unwrap_or("").to_owned())
                            .or_default() += 1;
                    }
                    for n in new.iter_mut() {
                        if n["provider_id"].as_str().is_some() {
                            n["url"] = json!("");
                            continue;
                        }
                        if !n["url"].as_str().unwrap_or("").is_empty() {
                            continue;
                        }
                        let by_id = n["id"].as_str().and_then(|nid| {
                            old.iter()
                                .find(|o| o["id"].as_str() == Some(nid) && o["kind"] == n["kind"])
                        });
                        let kind_name = n["kind"].as_str().unwrap_or("");
                        let by_kind = if n["id"].is_null()
                            && new_counts.get(kind_name) == Some(&1)
                            && old_counts.get(kind_name) == Some(&1)
                        {
                            old.iter().find(|o| o["kind"].as_str() == Some(kind_name))
                        } else {
                            None
                        };
                        if let Some(o) = by_id.or(by_kind) {
                            n["url"] = o["url"].clone();
                        }
                    }
                }
            }
            v["id"] = json!(id);
            v["created_at"] = prev["created_at"].clone();
        } else {
            return error(StatusCode::NOT_FOUND, "Object not found");
        }
    }
    if let Err(e) = validate(kind, &v) {
        return error(StatusCode::BAD_REQUEST, e);
    }
    if kind == "automations" {
        for (table, key) in [
            ("connections", "connection_id"),
            ("providers", "provider_id"),
        ] {
            if db::object(&s.db, table, v[key].as_str().unwrap_or(""))
                .await
                .ok()
                .flatten()
                .is_none()
            {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("Unknown {}", key.replace("_id", "")),
                );
            }
        }
        for n in v["notifications"].as_array().into_iter().flatten() {
            let Some(id) = n["provider_id"].as_str() else {
                continue;
            };
            let provider = db::object(&s.db, "notification_providers", id)
                .await
                .ok()
                .flatten();
            if provider.as_ref().is_none_or(|p| p["kind"] != n["kind"]) {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Notification provider is missing or its type does not match",
                );
            }
        }
    }
    if kind == "notification_providers" {
        if let Some(id) = object_id.as_deref() {
            let type_changed = db::object(&s.db, kind, id)
                .await
                .ok()
                .flatten()
                .is_some_and(|old| old["kind"] != v["kind"]);
            if type_changed
                && db::objects(&s.db, "automations").await.is_ok_and(|items| {
                    items.iter().any(|a| {
                        a["notifications"].as_array().is_some_and(|ns| {
                            ns.iter().any(|n| n["provider_id"].as_str() == Some(id))
                        })
                    })
                })
            {
                return error(
                    StatusCode::CONFLICT,
                    "Cannot change the type of a notification provider used by an automation",
                );
            }
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    if is_create {
        v["id"] = json!(uuid::Uuid::new_v4().to_string());
        v["created_at"] = json!(now.clone());
    }
    if kind == "automations" {
        if let Some(items) = v["notifications"].as_array_mut() {
            let mut ids = std::collections::HashSet::new();
            for item in items {
                if item["id"].as_str().is_none() {
                    item["id"] = json!(uuid::Uuid::new_v4().to_string())
                }
                if !ids.insert(item["id"].as_str().unwrap_or("").to_owned()) {
                    return error(StatusCode::BAD_REQUEST, "Notification ids must be unique");
                }
            }
        }
    }
    v["updated_at"] = json!(now);
    if let Err(e) = s.protect(kind, &mut v) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let mut result = v.clone();
    if kind == "automations" {
        let saved = if is_create {
            db::create_automation(&s.db, &v, s.license.limits().automations).await
        } else {
            db::update_automation(&s.db, &v).await
        };
        match saved {
            Ok(true) => (),
            Ok(false) if is_create => return error(StatusCode::FORBIDDEN, "Automation limit reached for this plan. Delete an automation or upgrade your self-hosted plan."),
            Ok(false) => return error(StatusCode::NOT_FOUND, "Automation not found"),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        }
    } else if let Err(e) = db::save_object(&s.db, kind, &v).await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if let Err(e) = s.mask(kind, &mut result) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    Json(result).into_response()
}
async fn create_connection(State(s): State<AppState>, h: HeaderMap, v: Json<Value>) -> Response {
    object_write(s, h, "connections", None, v).await
}
async fn update_connection(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    v: Json<Value>,
) -> Response {
    object_write(s, h, "connections", Some(id), v).await
}
async fn create_provider(State(s): State<AppState>, h: HeaderMap, v: Json<Value>) -> Response {
    object_write(s, h, "providers", None, v).await
}
async fn update_provider(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    v: Json<Value>,
) -> Response {
    object_write(s, h, "providers", Some(id), v).await
}
async fn create_notification_provider(
    State(s): State<AppState>,
    h: HeaderMap,
    v: Json<Value>,
) -> Response {
    object_write(s, h, "notification_providers", None, v).await
}
async fn update_notification_provider(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    v: Json<Value>,
) -> Response {
    object_write(s, h, "notification_providers", Some(id), v).await
}
async fn create_automation(State(s): State<AppState>, h: HeaderMap, v: Json<Value>) -> Response {
    object_write(s, h, "automations", None, v).await
}
async fn update_automation(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    v: Json<Value>,
) -> Response {
    object_write(s, h, "automations", Some(id), v).await
}
async fn object_delete(s: AppState, h: HeaderMap, kind: &str, id: String) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    if ["connections", "providers", "notification_providers"].contains(&kind) {
        let field = match kind {
            "connections" => "connection_id",
            "providers" => "provider_id",
            _ => "notification_provider_id",
        };
        if db::objects(&s.db, "automations").await.is_ok_and(|items| {
            items.iter().any(|a| {
                if kind == "notification_providers" {
                    a["notifications"]
                        .as_array()
                        .is_some_and(|ns| ns.iter().any(|n| n["provider_id"].as_str() == Some(&id)))
                } else {
                    a[field].as_str() == Some(&id)
                }
            })
        }) {
            return error(StatusCode::CONFLICT, "Object is used by an automation");
        }
    }
    match db::delete_object(&s.db, kind, &id).await {
        Ok(true) => (StatusCode::NO_CONTENT, ()).into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "Object not found"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
async fn delete_connection(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    object_delete(s, h, "connections", id).await
}
async fn delete_provider(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    object_delete(s, h, "providers", id).await
}
async fn delete_notification_provider(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    object_delete(s, h, "notification_providers", id).await
}
async fn delete_automation(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    object_delete(s, h, "automations", id).await
}

pub(crate) async fn run_queue(
    s: AppState,
    automation_id: String,
    trigger: Value,
) -> anyhow::Result<Value> {
    let automation = db::object(&s.db, "automations", &automation_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("automation not found"))?;
    if automation["enabled"] != true {
        return Err(anyhow::anyhow!("automation is disabled"));
    }
    let kind = trigger["kind"].as_str().unwrap_or("");
    let repo = trigger["repository"].as_str().unwrap_or("");
    if !automation["repositories"]
        .as_array()
        .is_some_and(|rs| rs.iter().any(|r| r.as_str() == Some(repo)))
    {
        anyhow::bail!("repository is outside this automation's configured repositories");
    }
    let action = automation["action"].as_str().unwrap_or("");
    if !["pull_request", "issue", "repository"].contains(&kind)
        || (action == "solve_issue" && kind != "issue")
        || (action == "fix_pr" && kind != "pull_request")
        || (kind != "repository" && trigger["number"].as_u64().is_none_or(|n| n == 0))
    {
        anyhow::bail!("run target is incompatible with the automation action");
    }
    if db::object(
        &s.db,
        "connections",
        automation["connection_id"].as_str().unwrap_or(""),
    )
    .await?
    .is_none()
        || db::object(
            &s.db,
            "providers",
            automation["provider_id"].as_str().unwrap_or(""),
        )
        .await?
        .is_none()
    {
        return Err(anyhow::anyhow!("automation configuration is incomplete"));
    }
    let id = if let Some(slot) = trigger["scheduled_at"].as_str() {
        use sha2::Digest;
        let identity = format!("{automation_id}|{repo}|{kind}|{}|{slot}", trigger["number"]);
        format!(
            "schedule-{}",
            &hex::encode(Sha256::digest(identity.as_bytes()))[..32]
        )
    } else if let Some(delivery) = trigger["delivery_identity"].as_str() {
        use sha2::Digest;
        let identity = format!("{automation_id}|{delivery}");
        format!(
            "webhook-{}",
            hex::encode(Sha256::digest(identity.as_bytes()))
        )
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    let now = chrono::Utc::now().to_rfc3339();
    let run = json!({"id":id,"automation_id":automation_id,"automation_name":automation["name"],"automation_version":automation["updated_at"],"status":"queued","trigger":trigger,"created_at":now,"started_at":null,"finished_at":null,"error":null,"output":null});
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO runs(id,automation_id,body,created_at,status) SELECT ?,?,?,?,? WHERE (SELECT COUNT(*) FROM runs WHERE status='queued') < 1000",
    )
    .bind(&id)
    .bind(&automation_id)
    .bind(run.to_string())
    .bind(&now)
    .bind("queued")
    .execute(&s.db)
    .await?;
    if inserted.rows_affected() == 0 {
        let saved: Option<String> = sqlx::query_scalar("SELECT body FROM runs WHERE id=?")
            .bind(&id)
            .fetch_optional(&s.db)
            .await?;
        return Ok(serde_json::from_str(&saved.ok_or_else(|| {
            anyhow::anyhow!("Run queue is full; retry later")
        })?)?);
    }
    Ok(run)
}
async fn persist_run(s: &AppState, v: &Value) -> anyhow::Result<()> {
    sqlx::query("UPDATE runs SET body=?,status=? WHERE id=?")
        .bind(v.to_string())
        .bind(v["status"].as_str().unwrap_or("queued"))
        .bind(v["id"].as_str().unwrap_or(""))
        .execute(&s.db)
        .await?;
    Ok(())
}
pub fn start_worker(state: AppState) {
    tokio::spawn(async move {
        if let Err(e) = recover_runs(&state).await {
            tracing::error!("run recovery failed: {e}")
        }
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            match process_queued(&state).await {
                Ok(true) => {}
                Ok(false) => {}
                Err(e) => {
                    tracing::error!("worker error: {e}");
                }
            }
        }
    });
}
async fn recover_runs(s: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id,body FROM runs WHERE status='running'")
        .fetch_all(&s.db)
        .await?;
    for row in rows {
        let mut run: Value = serde_json::from_str(&row.get::<String, _>("body"))?;
        run["status"] = json!("failed");
        run["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
        run["error"] = json!("Process restarted before run completed");
        persist_run(s, &run).await?;
    }
    Ok(())
}
async fn process_queued(s: &AppState) -> anyhow::Result<bool> {
    let update_guard = s.update_lock.lock().await;
    if *s.restart.borrow() {
        return Ok(false);
    }
    let mut tx = s.db.begin().await?;
    let row =
        sqlx::query("SELECT id,body FROM runs WHERE status='queued' ORDER BY created_at LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(false);
    };
    let id: String = row.get("id");
    let mut run: Value = serde_json::from_str(&row.get::<String, _>("body"))?;
    run["status"] = json!("running");
    run["started_at"] = json!(chrono::Utc::now().to_rfc3339());
    let update =
        sqlx::query("UPDATE runs SET status='running',body=? WHERE id=? AND status='queued'")
            .bind(run.to_string())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    tx.commit().await?;
    drop(update_guard);
    if update.rows_affected() == 0 {
        return Ok(false);
    }
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    s.cancellations.lock().await.insert(id.clone(), cancel_tx);
    let result = execute_run(s, &run, &mut cancel_rx).await;
    match result {
        Ok(output) => {
            run["status"] = json!("succeeded");
            run["output"] = serde_json::to_value(output)?
        }
        Err(e) => {
            let message = redact_error(s, &run, &e.to_string()).await;
            run["status"] = json!(if message == "Run cancelled" {
                "cancelled"
            } else {
                "failed"
            });
            run["error"] = json!(message)
        }
    }
    run["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
    persist_run(s, &run).await?;
    s.cancellations.lock().await.remove(&id);
    Ok(true)
}
async fn execute_run(
    s: &AppState,
    run: &Value,
    cancel_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<crate::types::RunOutput> {
    let mut automation = db::object(
        &s.db,
        "automations",
        run["automation_id"].as_str().unwrap_or(""),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("automation not found"))?;
    if automation["enabled"] != true {
        anyhow::bail!("automation was paused before execution")
    }
    if !run["automation_version"].is_null() && run["automation_version"] != automation["updated_at"]
    {
        anyhow::bail!(
            "automation changed after this run was queued; retry to use the new configuration"
        )
    }
    s.reveal("automations", &mut automation)?;
    resolve_notification_providers(s, &mut automation).await?;
    let mut conn = db::object(
        &s.db,
        "connections",
        automation["connection_id"].as_str().unwrap_or(""),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("connection not found"))?;
    s.reveal("connections", &mut conn)?;
    let mut provider = db::object(
        &s.db,
        "providers",
        automation["provider_id"].as_str().unwrap_or(""),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("provider not found"))?;
    s.reveal("providers", &mut provider)?;
    let timeout = automation["limits"]["timeout_seconds"]
        .as_u64()
        .unwrap_or(600)
        .clamp(1, 3600);
    let trigger = run["trigger"].clone();
    let context = crate::types::EngineContext {
        run_id: run["id"].as_str().unwrap_or("").to_owned(),
        automation,
        connection: conn,
        provider,
        trigger,
        client: s.client.clone(),
    };
    let future = crate::engine::execute(context);
    tokio::pin!(future);
    tokio::select! {_ = tokio::time::sleep(Duration::from_secs(timeout))=>Err(anyhow::anyhow!("Run timed out after {timeout} seconds")),_ = cancel_rx.changed()=>Err(anyhow::anyhow!("Run cancelled")),result=&mut future=>result}
}
async fn resolve_notification_providers(
    s: &AppState,
    automation: &mut Value,
) -> anyhow::Result<()> {
    let Some(notifications) = automation["notifications"].as_array_mut() else {
        return Ok(());
    };
    for notification in notifications {
        let Some(id) = notification["provider_id"].as_str().map(str::to_owned) else {
            continue;
        };
        let mut provider = db::object(&s.db, "notification_providers", &id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("notification provider not found"))?;
        s.reveal("notification_providers", &mut provider)?;
        anyhow::ensure!(
            provider["kind"] == notification["kind"],
            "notification provider type does not match automation"
        );
        notification["url"] = provider["url"].clone();
    }
    Ok(())
}
async fn redact_error(s: &AppState, run: &Value, error: &str) -> String {
    let mut secrets = vec![];
    if let Ok(Some(mut a)) = db::object(
        &s.db,
        "automations",
        run["automation_id"].as_str().unwrap_or(""),
    )
    .await
    {
        let _ = s.reveal("automations", &mut a);
        let _ = resolve_notification_providers(s, &mut a).await;
        for n in a["notifications"].as_array().into_iter().flatten() {
            if let Some(x) = n["url"].as_str() {
                if !x.is_empty() {
                    secrets.push(x.to_owned())
                }
            }
        }
        if let Ok(Some(mut c)) = db::object(
            &s.db,
            "connections",
            a["connection_id"].as_str().unwrap_or(""),
        )
        .await
        {
            let _ = s.reveal("connections", &mut c);
            for key in ["token", "webhook_secret"] {
                if let Some(x) = c[key].as_str() {
                    if !x.is_empty() {
                        secrets.push(x.to_owned())
                    }
                }
                if let Some(x) = c["base_url"].as_str() {
                    if !x.is_empty() {
                        secrets.push(x.to_owned())
                    }
                }
            }
        }
        if let Ok(Some(mut p)) =
            db::object(&s.db, "providers", a["provider_id"].as_str().unwrap_or("")).await
        {
            let _ = s.reveal("providers", &mut p);
            if let Some(x) = p["api_key"].as_str() {
                if !x.is_empty() {
                    secrets.push(x.to_owned())
                }
            }
            if let Some(x) = p["base_url"].as_str() {
                if !x.is_empty() {
                    secrets.push(x.to_owned())
                }
            }
        }
    }
    let mut out = error.to_owned();
    for secret in secrets {
        out = out.replace(&secret, "[redacted]")
    }
    out.chars().take(2000).collect()
}
async fn run_automation(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(trigger): Json<Value>,
) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    if !["pull_request", "issue", "repository"].contains(&trigger["kind"].as_str().unwrap_or("")) {
        return error(StatusCode::BAD_REQUEST, "Invalid run kind");
    }
    match run_queue(s, id, trigger).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}
async fn get_run(State(s): State<AppState>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if let Err(r) = guard(&s, &h, false).await {
        return r;
    }
    match sqlx::query_scalar::<_, String>("SELECT body FROM runs WHERE id=?")
        .bind(id)
        .fetch_optional(&s.db)
        .await
    {
        Ok(Some(b)) => match serde_json::from_str::<Value>(&b) {
            Ok(v) => Json(v).into_response(),
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Ok(None) => error(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
async fn list_runs(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(r) = guard(&s, &h, false).await {
        return r;
    }
    match sqlx::query_scalar::<_, String>(
        "SELECT body FROM runs ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&s.db)
    .await
    {
        Ok(rows) => {
            let vals: Result<Vec<Value>, _> =
                rows.iter().map(|r| serde_json::from_str(r)).collect();
            match vals {
                Ok(v) => Json(v).into_response(),
                Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
            }
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
async fn dashboard(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(r) = guard(&s, &h, false).await {
        return r;
    }
    let autos = db::objects(&s.db, "automations").await.unwrap_or_default();
    let recent: Vec<Value> = match sqlx::query_scalar::<_, String>(
        "SELECT body FROM runs ORDER BY created_at DESC LIMIT 8",
    )
    .fetch_all(&s.db)
    .await
    {
        Ok(x) => x
            .iter()
            .filter_map(|b| serde_json::from_str(b).ok())
            .collect(),
        Err(_) => vec![],
    };
    let runs = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runs")
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);
    let all: Vec<Value> = match sqlx::query_scalar::<_, String>("SELECT body FROM runs")
        .fetch_all(&s.db)
        .await
    {
        Ok(x) => x
            .iter()
            .filter_map(|b| serde_json::from_str(b).ok())
            .collect(),
        Err(_) => vec![],
    };
    let findings = all
        .iter()
        .map(|r| {
            r["output"]["findings"]
                .as_array()
                .map_or(0, |x| x.len() as i64)
        })
        .sum::<i64>();
    Json(json!({"automations":autos.len(),"active_automations":autos.iter().filter(|a|a["enabled"]==true).count(),"runs":runs,"findings":findings,"recent_runs":recent})).into_response()
}

async fn cancel_run(State(s): State<AppState>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    let map = s.cancellations.lock().await;
    if let Some(tx) = map.get(&id) {
        let _ = tx.send(true);
        return Json(json!({"ok":true})).into_response();
    }
    drop(map);
    let row = match sqlx::query("SELECT body FROM runs WHERE id=? AND status='queued'")
        .bind(&id)
        .fetch_optional(&s.db)
        .await
    {
        Ok(x) => x,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let Some(row) = row else {
        return error(StatusCode::CONFLICT, "Run is not queued or running");
    };
    let mut run: Value = match serde_json::from_str(&row.get::<String, _>("body")) {
        Ok(x) => x,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    run["status"] = json!("cancelled");
    run["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
    run["error"] = json!("Cancelled before execution");
    match persist_run(&s, &run).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
async fn retry_run(State(s): State<AppState>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    match sqlx::query_scalar::<_, String>("SELECT body FROM runs WHERE id=?")
        .bind(id)
        .fetch_optional(&s.db)
        .await
    {
        Ok(Some(b)) => match serde_json::from_str::<Value>(&b) {
            Ok(mut v) => {
                if matches!(v["status"].as_str(), Some("queued" | "running")) {
                    return error(
                        StatusCode::CONFLICT,
                        "Wait for the current run to finish before retrying",
                    );
                }
                if let Some(trigger) = v["trigger"].as_object_mut() {
                    trigger.remove("scheduled_at");
                    trigger.insert("source".into(), json!("retry"));
                }
                match run_queue(
                    s,
                    v["automation_id"].as_str().unwrap_or("").to_owned(),
                    v["trigger"].clone(),
                )
                .await
                {
                    Ok(r) => Json(r).into_response(),
                    Err(e) => error(StatusCode::BAD_REQUEST, e),
                }
            }
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Ok(None) => error(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct Preview {
    cron: String,
    timezone: String,
}
async fn schedule_preview(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Preview>,
) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    let normalized = match normalize_cron(&p.cron) {
        Ok(x) => x,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid cron expression"),
    };
    let schedule = match cron::Schedule::from_str(&normalized) {
        Ok(x) => x,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid cron expression"),
    };
    let tz = match p.timezone.parse::<chrono_tz::Tz>() {
        Ok(x) => x,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid timezone"),
    };
    let now = chrono::Utc::now().with_timezone(&tz);
    let next: Vec<String> = schedule
        .after(&now)
        .take(5)
        .map(|d| d.with_timezone(&chrono::Utc).to_rfc3339())
        .collect();
    Json(json!({"next":next})).into_response()
}

async fn test_connection(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    let mut c = match db::object(&s.db, "connections", &id).await {
        Ok(Some(x)) => x,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Connection not found"),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if let Err(e) = s.reveal("connections", &mut c) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    match crate::scm::test_connection(&c, &s.client).await {
        Ok(m) => Json(json!({"ok":true,"message":m})).into_response(),
        Err(e) => Json(json!({"ok":false,"message":e.to_string()})).into_response(),
    }
}
async fn test_provider(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    let mut p = match db::object(&s.db, "providers", &id).await {
        Ok(Some(x)) => x,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Provider not found"),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if let Err(e) = s.reveal("providers", &mut p) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    match crate::providers::test_provider(&p, &s.client).await {
        Ok(m) => Json(json!({"ok":true,"message":m})).into_response(),
        Err(e) => Json(json!({"ok":false,"message":e.to_string()})).into_response(),
    }
}
async fn test_notification_provider(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response {
    if let Err(r) = guard(&s, &h, true).await {
        return r;
    }
    let mut p = match db::object(&s.db, "notification_providers", &id).await {
        Ok(Some(x)) => x,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Notification provider not found"),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if let Err(e) = s.reveal("notification_providers", &mut p) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    match crate::notifications::send(
        &p,
        "Diffrook test notification. This channel is configured correctly.",
        &s.client,
    )
    .await
    {
        Ok(()) => Json(json!({"ok":true,"message":"Test notification sent"})).into_response(),
        Err(e) => Json(json!({"ok":false,"message":e.to_string()})).into_response(),
    }
}

async fn webhook(
    State(s): State<AppState>,
    Path(connection_id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let mut c = match db::object(&s.db, "connections", &connection_id).await {
        Ok(Some(x)) => x,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Connection not found"),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let forgejo = c["kind"].as_str() == Some("forgejo");
    let signature = headers
        .get(if forgejo {
            "x-forgejo-signature"
        } else {
            "x-hub-signature-256"
        })
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let delivery = headers
        .get(if forgejo {
            "x-forgejo-delivery"
        } else {
            "x-github-delivery"
        })
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let event = headers
        .get(if forgejo {
            "x-forgejo-event"
        } else {
            "x-github-event"
        })
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if signature.is_empty()
        || delivery.is_empty()
        || event.is_empty()
        || delivery.len() > 256
        || event.len() > 128
    {
        return error(
            StatusCode::UNAUTHORIZED,
            "Missing signature or delivery headers",
        );
    }
    if let Err(e) = s.reveal("connections", &mut c) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let secret = c["webhook_secret"].as_str().unwrap_or("");
    if secret.is_empty() {
        return error(StatusCode::UNAUTHORIZED, "Webhook secret is not configured");
    }
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(x) => x,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "Invalid webhook key"),
    };
    mac.update(&body);
    let digest = hex::encode(mac.finalize().into_bytes());
    let expected = if forgejo {
        digest
    } else {
        format!("sha256={digest}")
    };
    if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
        return error(StatusCode::UNAUTHORIZED, "Invalid webhook signature");
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(x) => x,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    let received = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM webhook_deliveries WHERE connection_id=? AND delivery_id=?",
    )
    .bind(&connection_id)
    .bind(delivery)
    .fetch_one(&s.db)
    .await;
    match received {
        Ok(1) => return Json(json!({"ok":true,"duplicate":true,"queued":0})).into_response(),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        _ => (),
    }
    let action = event_action(event, &payload);
    let Some((repo, kind, number, branch, actor)) = webhook_target(&payload, event) else {
        return Json(json!({"ok":true,"queued":0})).into_response();
    };
    let sender = payload["sender"]["login"]
        .as_str()
        .or_else(|| payload["sender"]["username"].as_str())
        .unwrap_or("");
    if sender.eq_ignore_ascii_case(c["bot_username"].as_str().unwrap_or("")) {
        return Json(json!({"ok":true,"queued":0,"ignored":"bot event"})).into_response();
    }
    let command = payload["comment"]["body"].as_str().unwrap_or("");
    let autos = match db::objects(&s.db, "automations").await {
        Ok(x) => x,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut queued = 0;
    for a in autos {
        if a["enabled"] != true
            || a["connection_id"].as_str() != Some(&connection_id)
            || !a["repositories"]
                .as_array()
                .is_some_and(|rs| rs.iter().any(|r| r.as_str() == Some(&repo)))
        {
            continue;
        }
        let events = a["trigger"]["events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !events.iter().any(|e| e.as_str() == Some(action)) {
            continue;
        }
        if (a["action"] == "solve_issue" && kind != "issue")
            || (a["action"] == "fix_pr" && kind != "pull_request")
        {
            continue;
        }
        if action == "issue_comment.command"
            && !matches_command(
                command,
                a["trigger"]["command"].as_str().unwrap_or("/diffrook"),
            )
        {
            continue;
        }
        let filters = &a["filters"];
        if filters["ignore_drafts"] == true && payload["pull_request"]["draft"] == true {
            continue;
        }
        let allowed = filters["allowed_actors"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let auth_actor = if action == "issue_comment.command" {
            sender
        } else {
            actor.as_str()
        };
        if !allowed.is_empty()
            && !allowed.iter().any(|x| {
                x.as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(auth_actor))
            })
        {
            continue;
        }
        if action == "issue_comment.command"
            && allowed.is_empty()
            && !matches!(
                payload["comment"]["author_association"].as_str().unwrap_or(
                    payload["sender"]["author_association"]
                        .as_str()
                        .unwrap_or("")
                ),
                "OWNER" | "MEMBER" | "COLLABORATOR"
            )
        {
            continue;
        }
        if auth_actor.is_empty()
            || auth_actor.eq_ignore_ascii_case(c["bot_username"].as_str().unwrap_or(""))
        {
            continue;
        }
        let configured_labels = filters["labels"].as_array().cloned().unwrap_or_default();
        if !configured_labels.is_empty() {
            let labels = payload["pull_request"]["labels"]
                .as_array()
                .or(payload["issue"]["labels"].as_array());
            if !labels.is_some_and(|actual| {
                actual.iter().any(|l| {
                    configured_labels.iter().any(|f| {
                        f.as_str().is_some_and(|x| {
                            x.eq_ignore_ascii_case(l["name"].as_str().or(l.as_str()).unwrap_or(""))
                        })
                    })
                })
            }) {
                continue;
            }
        }
        let id = a["id"].as_str().unwrap_or("").to_owned();
        let tr = json!({"kind":kind,"repository":repo,"number":number,"branch":branch,"event":event,"actor":actor,"payload":payload,"delivery_identity":format!("{connection_id}|{delivery}")});
        match run_queue(s.clone(), id, tr).await {
            Ok(_) => queued += 1,
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Webhook could not be queued; retry this delivery",
                )
            }
        }
    }
    // Mark complete only after all matching jobs are durable. Stable run IDs make
    // retries safe even if an earlier attempt queued only some of the automations.
    if let Err(e) = sqlx::query("INSERT OR IGNORE INTO webhook_deliveries(connection_id,delivery_id,created_at) VALUES(?,?,?)").bind(&connection_id).bind(delivery).bind(chrono::Utc::now().to_rfc3339()).execute(&s.db).await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    Json(json!({"ok":true,"queued":queued})).into_response()
}
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}
fn event_action(event: &str, p: &Value) -> &'static str {
    let event = match event {
        "pull_request.synchronized" => "pull_request",
        "pull_request" => "pull_request",
        _ => event,
    };
    match (event, p["action"].as_str().unwrap_or("")) {
        ("pull_request", "opened") => "pull_request.opened",
        ("pull_request", "synchronize" | "synchronized") => "pull_request.synchronize",
        ("pull_request", "ready_for_review") => "pull_request.ready_for_review",
        ("issues", "opened") => "issues.opened",
        ("issues", "labeled") => "issues.labeled",
        ("issue_comment", "created") => "issue_comment.command",
        _ => "",
    }
}
fn matches_command(body: &str, command: &str) -> bool {
    let line = body.trim_start();
    line.get(..command.len())
        .is_some_and(|x| x.eq_ignore_ascii_case(command))
        && line[command.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}
type WebhookTarget = (String, &'static str, Option<Value>, Option<String>, String);
fn webhook_target(p: &Value, event: &str) -> Option<WebhookTarget> {
    let repo = p["repository"]["full_name"]
        .as_str()
        .or_else(|| p["repository"]["name"].as_str())?
        .to_owned();
    let actor = p["sender"]["login"]
        .as_str()
        .or_else(|| p["sender"]["username"].as_str())
        .unwrap_or("")
        .to_owned();
    let (kind, n, branch) =
        if event.starts_with("pull_request") || p["issue"]["pull_request"].is_object() {
            (
                "pull_request",
                p["pull_request"]["number"]
                    .as_number()
                    .cloned()
                    .or_else(|| p["issue"]["number"].as_number().cloned())
                    .map(|n| json!(n)),
                p["pull_request"]["head"]["ref"].as_str().map(str::to_owned),
            )
        } else {
            (
                "issue",
                p["issue"]["number"].as_number().cloned().map(|n| json!(n)),
                None,
            )
        };
    Some((repo, kind, n, branch, actor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Timelike};
    #[tokio::test]
    async fn update_restart_prevents_claiming_queued_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path()).await.unwrap();
        let update_guard = state.update_lock.lock().await;
        let worker_state = state.clone();
        let worker = tokio::spawn(async move { process_queued(&worker_state).await });
        state.restart.send_replace(true);
        drop(update_guard);
        assert!(!worker.await.unwrap().unwrap());
    }
    #[test]
    fn five_field_cron_uses_standard_weekday_numbers() {
        let normalized = normalize_cron("0 9 * * 1").unwrap();
        assert_eq!(normalized, "0 0 9 * * MON");
        let schedule = cron::Schedule::from_str(&normalized).unwrap();
        let tz = "Europe/Lisbon".parse::<chrono_tz::Tz>().unwrap();
        let before = tz.with_ymd_and_hms(2025, 1, 5, 10, 0, 0).unwrap();
        let next = schedule.after(&before).next().unwrap();
        assert_eq!(next.weekday(), chrono::Weekday::Mon);
        assert_eq!(next.hour(), 9);
        assert_eq!(normalize_cron("0 9 * * MON").unwrap(), normalized);
        for pattern in ["0 9 * * 1-7", "0 9 * * MON-FRI", "0 9 * * */2"] {
            assert!(cron::Schedule::from_str(&normalize_cron(pattern).unwrap()).is_ok());
        }
    }
    #[test]
    fn comment_command_requires_exact_token_prefix() {
        assert!(matches_command("/diffrook please review", "/diffrook"));
        assert!(!matches_command("/diffrooking please review", "/diffrook"));
        assert!(!matches_command("hello /diffrook", "/diffrook"));
    }
    #[tokio::test]
    async fn interrupted_runs_are_persistently_marked_failed() {
        let dir = tempfile::tempdir().unwrap();
        let s = AppState::new(dir.path()).await.unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let run = json!({"id":"r1","automation_id":"a1","status":"running","created_at":now,"finished_at":null,"error":null});
        sqlx::query("INSERT INTO runs(id,automation_id,body,created_at,status) VALUES('r1','a1',?,?, 'running')").bind(run.to_string()).bind(&now).execute(&s.db).await.unwrap();
        recover_runs(&s).await.unwrap();
        let body: String = sqlx::query_scalar("SELECT body FROM runs WHERE id='r1'")
            .fetch_one(&s.db)
            .await
            .unwrap();
        let recovered: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(recovered["status"], "failed");
        assert_eq!(recovered["error"], "Process restarted before run completed");
    }
}
