use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    value: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-diffrook-request", "1");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(
                    value
                        .map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

async fn fixture() -> (
    tempfile::TempDir,
    diffrook::core::AppState,
    axum::Router,
    String,
    Value,
) {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::with_security(dir.path(), Default::default())
        .await
        .unwrap();
    let app = diffrook::routes::router(state.clone());
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/setup")
        .header("content-type","application/json").header("x-diffrook-request","1")
        .body(Body::from(json!({"username":"personal","password":"test-personal-password","setup_token":state.setup_token.as_ref()}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for (kind, object) in [
        (
            "connections",
            json!({"id":"c","name":"Mock","kind":"github"}),
        ),
        ("providers", json!({"id":"p","name":"Mock","kind":"openai"})),
    ] {
        let mut object = object;
        object["created_at"] = json!("now");
        object["updated_at"] = json!("now");
        diffrook::db::save_object(&state.db, kind, &object)
            .await
            .unwrap();
    }
    let automation = json!({"name":"Quota probe","enabled":false,"connection_id":"c","provider_id":"p","repositories":["acme/demo"],"action":"review","trigger":{},"filters":{},"limits":{"max_files":200,"max_file_bytes":64000,"max_context_chars":180000,"max_output_tokens":6000,"timeout_seconds":60,"max_fix_files":10},"fix":{"mode":"new_branch"},"notifications":[]});
    (dir, state, app, cookie, automation)
}

async fn write_with_headers(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("x-diffrook-request", "1")
                .header("cookie", cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn star_invitation_only_follows_the_first_successful_creation_and_survives_restart() {
    let (dir, state, app, cookie, input) = fixture().await;
    let failed = write_with_headers(&app, &cookie, "POST", "/api/automations", &json!({})).await;
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);
    assert!(!failed.headers().contains_key("x-diffrook-star-prompt"));
    let first = write_with_headers(&app, &cookie, "POST", "/api/automations", &input).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["x-diffrook-star-prompt"], "1");
    let saved: Value =
        serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let path = format!("/api/automations/{}", saved["id"].as_str().unwrap());
    let edited = write_with_headers(&app, &cookie, "PUT", &path, &saved).await;
    assert_eq!(edited.status(), StatusCode::OK);
    assert!(!edited.headers().contains_key("x-diffrook-star-prompt"));
    let next = write_with_headers(&app, &cookie, "POST", "/api/automations", &input).await;
    assert_eq!(next.status(), StatusCode::OK);
    assert!(!next.headers().contains_key("x-diffrook-star-prompt"));
    for object in diffrook::db::objects(&state.db, "automations")
        .await
        .unwrap()
    {
        diffrook::db::delete_object(&state.db, "automations", object["id"].as_str().unwrap())
            .await
            .unwrap();
    }
    let reloaded = diffrook::core::AppState::with_security(dir.path(), Default::default())
        .await
        .unwrap();
    let after_restart = write_with_headers(
        &diffrook::routes::router(reloaded),
        &cookie,
        "POST",
        "/api/automations",
        &input,
    )
    .await;
    assert_eq!(after_restart.status(), StatusCode::OK);
    assert!(!after_restart
        .headers()
        .contains_key("x-diffrook-star-prompt"));
}

#[tokio::test]
async fn concurrent_creations_offer_exactly_one_invitation_and_preserve_the_quota() {
    let (_dir, state, _app, _cookie, _) = fixture().await;
    let rejected = json!({"id":"rejected","created_at":"now","updated_at":"now"});
    assert_eq!(
        diffrook::db::create_automation_with_onboarding(&state.db, &rejected, Some(0))
            .await
            .unwrap(),
        (false, false)
    );
    let mut tasks = Vec::new();
    for n in 0..12 {
        let pool = state.db.clone();
        tasks.push(tokio::spawn(async move {
            let value = json!({"id":format!("parallel-{n}"),"created_at":"now","updated_at":"now"});
            diffrook::db::create_automation_with_onboarding(&pool, &value, Some(3))
                .await
                .unwrap()
        }));
    }
    let (mut created, mut invited) = (0, 0);
    for task in tasks {
        let (saved, prompt) = task.await.unwrap();
        created += usize::from(saved);
        invited += usize::from(prompt);
    }
    assert_eq!((created, invited), (3, 1));
}

#[tokio::test]
async fn existing_installations_do_not_receive_a_first_creation_invitation() {
    let (_dir, state, _app, _cookie, _) = fixture().await;
    let legacy = json!({"id":"legacy","created_at":"now","updated_at":"now"});
    diffrook::db::save_object(&state.db, "automations", &legacy)
        .await
        .unwrap();
    let new = json!({"id":"new","created_at":"now","updated_at":"now"});
    assert_eq!(
        diffrook::db::create_automation_with_onboarding(&state.db, &new, None)
            .await
            .unwrap(),
        (true, false)
    );
}

#[tokio::test]
async fn individual_api_counts_disabled_automations_and_allows_edit_delete_and_replacement() {
    let (_dir, state, app, cookie, input) = fixture().await;
    assert_eq!(
        call(&app, "GET", "/api/license", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let mut saved = vec![];
    for _ in 0..3 {
        let (code, value) = call(
            &app,
            "POST",
            "/api/automations",
            Some(input.clone()),
            Some(&cookie),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{value}");
        saved.push(value);
    }
    let mut forged = input.clone();
    forged["plan"] = json!("business");
    forged["max_automations"] = json!(999);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/automations",
            Some(forged),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let status = call(&app, "GET", "/api/license", None, Some(&cookie))
        .await
        .1;
    assert_eq!(status["limits"], json!({"users":1,"automations":3}));
    assert_eq!(status["usage"], json!({"users":1,"automations":3}));
    assert_eq!(status["can_create_automation"], false);
    saved[0]["name"] = json!("Edited at quota");
    let path = format!("/api/automations/{}", saved[0]["id"].as_str().unwrap());
    assert_eq!(
        call(&app, "PUT", &path, Some(saved[0].clone()), Some(&cookie))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "DELETE", &path, None, Some(&cookie)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "PUT", &path, Some(saved[0].clone()), Some(&cookie))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "POST", "/api/automations", Some(input), Some(&cookie))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(state.license.limits().automations, Some(3));
}

#[tokio::test]
async fn beta_plan_selection_requires_authentication_and_enforces_real_allowances() {
    let (dir, state, app, cookie, input) = fixture().await;
    if diffrook::licensing::catalog()["beta"]["enabled"] != true {
        let catalog = diffrook::licensing::catalog();
        assert_eq!(catalog["self_hosted_status"], "available");
        assert_eq!(catalog["default_self_hosted_plan"], "individual");
        assert_eq!(catalog["self_hosted"][0]["status"], "available");
        assert_eq!(catalog["self_hosted"][0]["default"], true);
        assert_eq!(
            state.license.status(&state.db).await.unwrap()["edition"],
            "individual"
        );
        for plan in catalog["self_hosted"].as_array().unwrap().iter().skip(1) {
            assert_eq!(plan["status"], "coming_soon");
        }
        assert_eq!(catalog["self_hosted_purchase_available"], false);
        for edition in ["freelancer", "teams", "individual", "enterprise"] {
            assert_eq!(
                call(
                    &app,
                    "POST",
                    "/api/license/beta-plan",
                    Some(json!({"edition":edition})),
                    Some(&cookie)
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/license/beta-plan",
                Some(json!({"edition":"teams"})),
                Some(&cookie)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        return;
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/license/beta-plan",
            Some(json!({"edition":"teams"})),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for excluded in ["enterprise", "cloud", "business"] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/license/beta-plan",
                Some(json!({"edition":excluded})),
                Some(&cookie)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    // Teams allows 20 automations for each of its up to 10 users.
    for (edition, max, sso) in [("freelancer", 10, false), ("teams", 200, true)] {
        let (code, status) = call(
            &app,
            "POST",
            "/api/license/beta-plan",
            Some(json!({"edition":edition})),
            Some(&cookie),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(status["features"]["sso"], sso);
        assert_eq!(status["beta_free"], true);
        for _ in 0..max {
            if state.license.status(&state.db).await.unwrap()["usage"]["automations"]
                .as_i64()
                .unwrap()
                == max
            {
                break;
            }
            assert_eq!(
                call(
                    &app,
                    "POST",
                    "/api/automations",
                    Some(input.clone()),
                    Some(&cookie)
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/automations",
                Some(input.clone()),
                Some(&cookie)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let reloaded = diffrook::core::AppState::with_security(dir.path(), Default::default())
        .await
        .unwrap();
    assert_eq!(
        reloaded.license.status(&reloaded.db).await.unwrap()["edition"],
        "teams"
    );
    let (_, status) = call(
        &app,
        "POST",
        "/api/license/beta-plan",
        Some(json!({"edition":"individual"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status["over_limit"], true);
    assert_eq!(status["usage"]["automations"], 200);
    assert_eq!(status["commercial_use"], false);
    assert_eq!(status["features"]["sso"], false);
}

#[tokio::test]
async fn parallel_api_creations_cannot_exceed_the_individual_quota() {
    let (_dir, state, app, cookie, input) = fixture().await;
    let mut pending = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let (app, cookie, input) = (app.clone(), cookie.clone(), input.clone());
        pending.spawn(async move {
            call(&app, "POST", "/api/automations", Some(input), Some(&cookie))
                .await
                .0
        });
    }
    let mut created = 0;
    while let Some(result) = pending.join_next().await {
        match result.unwrap() {
            StatusCode::OK => created += 1,
            StatusCode::FORBIDDEN => (),
            other => panic!("Unexpected response: {other}"),
        }
    }
    assert_eq!(created, 3);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM objects WHERE kind='automations'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn existing_over_quota_data_is_preserved_and_reported_without_allowing_more() {
    let (_dir, state, app, cookie, mut input) = fixture().await;
    input["created_at"] = json!("now");
    input["updated_at"] = json!("now");
    for n in 0..4 {
        input["id"] = json!(format!("legacy-{n}"));
        diffrook::db::save_object(&state.db, "automations", &input)
            .await
            .unwrap();
    }
    let status = call(&app, "GET", "/api/license", None, Some(&cookie))
        .await
        .1;
    assert_eq!(status["over_limit"], true);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/automations",
            Some(input.clone()),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/automations/legacy-3",
            Some(input),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/api/automations", None, Some(&cookie))
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[tokio::test]
async fn public_catalog_lists_approved_prices_but_never_offers_cloud_activation() {
    let (_dir, _state, app, _cookie, _input) = fixture().await;
    let (code, catalog) = call(&app, "GET", "/api/plans", None, None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(catalog["currency"], "EUR");
    assert_eq!(catalog["self_hosted"][0]["free_forever"], true);
    assert_eq!(catalog["self_hosted"][1]["monthly_eur"], 5.99);
    assert_eq!(catalog["self_hosted"][2]["monthly_eur"], 9.99);
    assert_eq!(catalog["self_hosted"][2]["per_user"], true);
    assert_eq!(catalog["self_hosted"][2]["users"], 10);
    assert_eq!(catalog["self_hosted"][2]["automations_per_user"], 20);
    assert_eq!(catalog["self_hosted"][3]["min_users"], 10);
    assert_eq!(catalog["self_hosted"][3]["contact_sales"], true);
    assert_eq!(catalog["cloud"]["status"], "coming_soon");
    assert_eq!(catalog["cloud"]["purchase_available"], false);
    for plan in catalog["cloud"]["plans"].as_array().unwrap() {
        assert!(plan["contact_sales"] == true || plan["monthly_eur"].as_f64().unwrap() > 0.0);
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/plans/cloud/activate",
            Some(json!({"plan":"teams"})),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
