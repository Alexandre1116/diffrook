use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{header, HeaderMap, Request, StatusCode},
};
use diffrook::{
    core::AppState,
    security::{OidcConfig, SecurityConfig},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use tower::ServiceExt;

fn public_config() -> SecurityConfig {
    SecurityConfig {
        public_url: Some(url::Url::parse("https://review.example.test").unwrap()),
        secure_cookies: true,
        ..Default::default()
    }
}
fn request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "review.example.test")
        .header(header::CONTENT_TYPE, "application/json")
}

#[tokio::test]
async fn public_authentication_enforces_origin_csrf_cookie_security_and_logout_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_security(dir.path(), public_config())
        .await
        .unwrap();
    let app = diffrook::routes::router(state.clone());
    let setup = json!({"username":"admin","password":"test-administrator-password","setup_token":state.setup_token.as_ref()});
    for path in ["/api/setup", "/api/login", "/api/logout"] {
        let response = app
            .clone()
            .oneshot(
                request("POST", path)
                    .body(Body::from(setup.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        for origin in [
            "https://attacker.example.test",
            "http://review.example.test",
            "https://review.example.test:444",
            "null",
        ] {
            let response = app
                .clone()
                .oneshot(
                    request("POST", path)
                        .header("x-diffrook-request", "1")
                        .header(header::ORIGIN, origin)
                        .body(Body::from(setup.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "accepted {origin} on {path}"
            );
        }
    }
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/setup")
                .header("x-diffrook-request", "1")
                .header(header::ORIGIN, "https://review.example.test")
                .body(Body::from(setup.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.starts_with("__Host-diffrook_session="));
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Secure")
            && cookie.contains("Max-Age=28800")
    );
    assert!(!cookie.contains("Domain="));
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/logout")
                .header("cookie", &cookie)
                .header("x-diffrook-request", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut headers = HeaderMap::new();
    headers.insert("cookie", cookie.parse().unwrap());
    assert!(diffrook::auth::authenticate(&state, &headers)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn incorrect_host_and_spoofed_forwarded_headers_do_not_bypass_limits() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_security(dir.path(), public_config())
        .await
        .unwrap();
    let app = diffrook::routes::router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header("host", "attacker.example.test")
                .header("x-forwarded-host", "review.example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    for attempt in 0..31 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/auth/oidc/start")
                    .header("host", "review.example.test")
                    .header("x-real-ip", format!("192.0.2.{}", attempt + 1))
                    .extension(ConnectInfo(
                        "198.51.100.10:1234".parse::<SocketAddr>().unwrap(),
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt < 30 {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }
}

#[tokio::test]
async fn sso_only_disables_setup_passwords_and_existing_local_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let initial = AppState::with_security(dir.path(), SecurityConfig::default())
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash,created_at) VALUES('local','admin','unused','now')").execute(&initial.db).await.unwrap();
    let token = "existing-session";
    use sha2::{Digest, Sha256};
    sqlx::query("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?,'local',?)")
        .bind(hex::encode(Sha256::digest(token.as_bytes())))
        .bind(chrono::Utc::now().timestamp() + 3600)
        .execute(&initial.db)
        .await
        .unwrap();
    // Exercise migration from the previous schema while preserving stored sessions.
    sqlx::query("ALTER TABLE sessions DROP COLUMN auth_method")
        .execute(&initial.db)
        .await
        .unwrap();
    initial.db.close().await;
    let mut config = public_config();
    config.local_login = false;
    config.oidc = Some(OidcConfig {
        issuer: "https://id.example.test".into(),
        client_id: "client".into(),
        client_secret: "mock-secret".into(),
        allowed_subjects: vec!["admin-subject".into()],
        allowed_emails: vec![],
    });
    let state = AppState::with_security(dir.path(), config).await.unwrap();
    assert!(state.setup_token.is_empty());
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        format!("__Host-diffrook_session={token}").parse().unwrap(),
    );
    assert!(diffrook::auth::authenticate(&state, &headers)
        .await
        .unwrap()
        .is_none());
    let app = diffrook::routes::router(state.clone());
    for path in ["/api/setup", "/api/login"] {
        let body = json!({"username":"admin","password":"administrator-password","setup_token":state.setup_token.as_ref()});
        let response = app
            .clone()
            .oneshot(
                request("POST", path)
                    .header("x-diffrook-request", "1")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = app
        .oneshot(request("GET", "/api/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["setup_required"], false);
    assert_eq!(body["sso_enabled"], false);
    assert_eq!(body["sso_requires_license"], true);
    assert_eq!(
        body["installation_id"],
        state.license.installation_id.to_string()
    );
    assert_eq!(body["local_login_enabled"], false);
}

#[tokio::test]
async fn oversized_authentication_and_database_failure_are_rejected_without_internal_details() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_security(dir.path(), SecurityConfig::default())
        .await
        .unwrap();
    let app = diffrook::routes::router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("content-type", "application/json")
                .header("x-diffrook-request", "1")
                .body(Body::from(
                    json!({"username":"a","password":"x".repeat(17000)}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    state.db.close().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header("x-diffrook-request", "1")
                .header("cookie", "diffrook_session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "Internal server error");
}

async fn webhook_fixture(state: &AppState) {
    for (kind, mut object) in [
        (
            "connections",
            json!({"id":"connection","kind":"github","token":"mock-token","webhook_secret":"mock-webhook-secret","bot_username":"bot"}),
        ),
        (
            "providers",
            json!({"id":"provider","kind":"openai","default_model":"mock"}),
        ),
        (
            "automations",
            json!({"id":"a1","name":"First","enabled":true,"connection_id":"connection","provider_id":"provider","repositories":["acme/demo"],"action":"review","trigger":{"events":["pull_request.opened"]}}),
        ),
        (
            "automations",
            json!({"id":"a2","name":"Second","enabled":true,"connection_id":"connection","provider_id":"provider","repositories":["acme/demo"],"action":"review","trigger":{"events":["pull_request.opened"]}}),
        ),
    ] {
        object["created_at"] = json!(if object["id"] == "a1" { "2" } else { "1" });
        object["updated_at"] = json!("1");
        state.protect(kind, &mut object).unwrap();
        diffrook::db::save_object(&state.db, kind, &object)
            .await
            .unwrap();
    }
}
async fn deliver(app: &axum::Router, delivery: &str) -> (StatusCode, Value) {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let body = json!({"action":"opened","repository":{"full_name":"acme/demo"},"pull_request":{"number":1,"head":{"ref":"feature"}},"sender":{"login":"developer"}}).to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(b"mock-webhook-secret").unwrap();
    mac.update(body.as_bytes());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/webhooks/connection")
                .header("x-github-event", "pull_request")
                .header("x-github-delivery", delivery)
                .header(
                    "x-hub-signature-256",
                    format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, body)
}

#[tokio::test]
async fn webhook_ignores_automations_for_incompatible_target_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_security(dir.path(), SecurityConfig::default())
        .await
        .unwrap();
    webhook_fixture(&state).await;
    let mut automation = diffrook::db::object(&state.db, "automations", "a2")
        .await
        .unwrap()
        .unwrap();
    automation["action"] = json!("solve_issue");
    diffrook::db::save_object(&state.db, "automations", &automation)
        .await
        .unwrap();
    let app = diffrook::routes::router(state.clone());
    let (status, body) = deliver(&app, "pr-with-issue-automation").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"], 1);
    let automation_ids: Vec<String> = sqlx::query_scalar("SELECT automation_id FROM runs")
        .fetch_all(&state.db)
        .await
        .unwrap();
    assert_eq!(automation_ids, vec!["a1"]);
}

#[tokio::test]
async fn partial_webhook_enqueue_failure_can_be_retried_without_duplicate_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_security(dir.path(), SecurityConfig::default())
        .await
        .unwrap();
    webhook_fixture(&state).await;
    let app = diffrook::routes::router(state.clone());
    sqlx::query("CREATE TRIGGER fail_second_run BEFORE INSERT ON runs WHEN NEW.automation_id='a2' BEGIN SELECT RAISE(FAIL,'simulated queue failure'); END").execute(&state.db).await.unwrap();
    assert_eq!(
        deliver(&app, "delivery-1").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(&state.db)
        .await
        .unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(runs, 1);
    assert_eq!(receipts, 0);
    sqlx::query("DROP TRIGGER fail_second_run")
        .execute(&state.db)
        .await
        .unwrap();
    let (status, body) = deliver(&app, "delivery-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"], 2);
    let (_, duplicate) = deliver(&app, "delivery-1").await;
    assert_eq!(duplicate["duplicate"], true);
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(runs, 2);
}

#[tokio::test]
async fn full_run_queue_rejects_webhooks_without_marking_them_received() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_security(dir.path(), SecurityConfig::default())
        .await
        .unwrap();
    webhook_fixture(&state).await;
    sqlx::query("WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<1000) INSERT INTO runs(id,automation_id,body,created_at,status) SELECT 'existing-'||n,'a1','{}','now','queued' FROM ids").execute(&state.db).await.unwrap();
    let app = diffrook::routes::router(state.clone());
    assert_eq!(
        deliver(&app, "queue-full").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    sqlx::query("DELETE FROM runs WHERE id IN ('existing-1','existing-2')")
        .execute(&state.db)
        .await
        .unwrap();
    assert_eq!(deliver(&app, "queue-full").await.0, StatusCode::OK);
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status='queued'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(runs, 1000);
}
