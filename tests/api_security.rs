use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-diffrook-request", "1");
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|h| h.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json, cookie)
}

async fn admin(app: &Router, state: &diffrook::core::AppState) -> String {
    let (status, _, cookie) = request(app, "POST", "/api/setup", Some(json!({"username":"admin","password":"this-is-a-test-password","setup_token":state.setup_token.as_ref()})), None).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

#[tokio::test]
async fn authentication_and_secret_storage_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::new(dir.path()).await.unwrap();
    let app = diffrook::routes::router(state.clone());
    assert_eq!(
        request(&app, "GET", "/api/connections", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = admin(&app, &state).await;
    let input = json!({"name":"Test","kind":"github","base_url":"https://api.github.com","token":"repository-test-secret","webhook_secret":"webhook-test-secret","bot_username":"bot"});
    let (status, saved, _) =
        request(&app, "POST", "/api/connections", Some(input), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["token"], "");
    assert_eq!(saved["webhook_secret"], "");
    assert_eq!(saved["has_token"], true);
    let mut stored = diffrook::db::object(&state.db, "connections", saved["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.to_string().contains("repository-test-secret"));
    state.reveal("connections", &mut stored).unwrap();
    assert_eq!(stored["token"], "repository-test-secret");
    let mut edited = saved.clone();
    edited["name"] = json!("Renamed");
    assert_eq!(
        request(
            &app,
            "PUT",
            &format!("/api/connections/{}", saved["id"].as_str().unwrap()),
            Some(edited),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut preserved =
        diffrook::db::object(&state.db, "connections", saved["id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
    state.reveal("connections", &mut preserved).unwrap();
    assert_eq!(preserved["token"], "repository-test-secret");
}

#[tokio::test]
async fn malformed_objects_return_errors_without_panics() {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::new(dir.path()).await.unwrap();
    let app = diffrook::routes::router(state.clone());
    let cookie = admin(&app, &state).await;
    let (_, connection, _) = request(
        &app,
        "POST",
        "/api/connections",
        Some(json!({"name":"Test", "kind":"github"})),
        Some(&cookie),
    )
    .await;
    let path = format!("/api/connections/{}", connection["id"].as_str().unwrap());
    for value in [json!([]), json!("oops"), json!(12), Value::Null] {
        assert_eq!(
            request(&app, "PUT", &path, Some(value.clone()), Some(&cookie))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(&app, "POST", "/api/connections", Some(value), Some(&cookie))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn missing_encryption_key_requires_restoring_backup() {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::new(dir.path()).await.unwrap();
    state.db.close().await;
    tokio::fs::remove_file(dir.path().join("encryption.key"))
        .await
        .unwrap();
    assert!(diffrook::core::AppState::new(dir.path()).await.is_err());
}

#[test]
fn csrf_origin_includes_port_and_ipv6_authority() {
    use axum::http::HeaderMap;
    let mut headers = HeaderMap::new();
    headers.insert("x-diffrook-request", "1".parse().unwrap());
    headers.insert("host", "localhost:8080".parse().unwrap());
    headers.insert("origin", "http://localhost:9090".parse().unwrap());
    assert!(!diffrook::core::request_origin_ok(&headers));
    headers.insert("origin", "http://localhost:8080".parse().unwrap());
    assert!(diffrook::core::request_origin_ok(&headers));
    headers.insert("host", "[::1]:8080".parse().unwrap());
    headers.insert("origin", "http://[::1]:8080".parse().unwrap());
    assert!(diffrook::core::request_origin_ok(&headers));
}

#[tokio::test]
async fn setup_token_cannot_register_a_second_administrator() {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::new(dir.path()).await.unwrap();
    let app = diffrook::routes::router(state.clone());
    let bad = json!({"username":"attacker","password":"this-is-a-test-password","setup_token":"incorrect"});
    assert_eq!(
        request(&app, "POST", "/api/setup", Some(bad), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let _cookie = admin(&app, &state).await;
    let another = json!({"username":"another","password":"this-is-a-test-password","setup_token":state.setup_token.as_ref()});
    assert_eq!(
        request(&app, "POST", "/api/setup", Some(another), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn update_policy_is_authenticated_and_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::new(dir.path()).await.unwrap();
    let app = diffrook::routes::router(state.clone());
    assert_eq!(
        request(&app, "GET", "/api/updates", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = admin(&app, &state).await;
    let (status, updates, _) = request(&app, "GET", "/api/updates", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updates["policy"], "manual");
    assert_eq!(updates["channel"], "stable");
    for (channel, expected) in [
        ("beta", StatusCode::OK),
        ("nightly", StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/updates/channel",
                Some(json!({ "channel": channel })),
                Some(&cookie)
            )
            .await
            .0,
            expected
        );
    }
    let (_, updates, _) = request(&app, "GET", "/api/updates", None, Some(&cookie)).await;
    assert_eq!(updates["channel"], "beta");
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/updates/policy",
            Some(json!({"mode":"automatic"})),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
    let saved: String =
        sqlx::query_scalar("SELECT value FROM app_settings WHERE key='updates.policy'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(saved, "automatic");
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/updates/policy",
            Some(json!({"mode":"surprise"})),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn strict_inputs_reject_shadow_fields_and_internal_urls_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let state = diffrook::core::AppState::new(dir.path()).await.unwrap();
    let app = diffrook::routes::router(state.clone());
    let cookie = admin(&app, &state).await;
    for (kind, body) in [
        (
            "connections",
            json!({"name":"probe","kind":"github","is_admin":true}),
        ),
        (
            "connections",
            json!({"name":"probe","kind":"github","__proto__":{"admin":true}}),
        ),
        (
            "providers",
            json!({"name":"probe","kind":"openai","default_model":"m","base_url":"https://example.com","is_admin":true}),
        ),
        (
            "notification-providers",
            json!({"name":"probe","kind":"webhook","url":"https://example.com","admin":true}),
        ),
        (
            "automations",
            json!({"trigger":{"__proto__":{"admin":true}}}),
        ),
        ("automations", json!({"limits":{"max_files":"200"}})),
        ("automations", json!({"trigger":[]})),
        ("automations", json!({"notifications":[[]]})),
        (
            "automations",
            json!({"notifications":[{"kind":"webhook","is_admin":true}]}),
        ),
    ] {
        let (status, _, _) = request(
            &app,
            "POST",
            &format!("/api/{kind}"),
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind}");
    }
    for base in [
        "http://127.0.0.1:8080",
        "http://[::1]",
        "http://169.254.169.254",
        "http://10.0.0.1",
        "http://host.docker.internal",
    ] {
        for (kind, body) in [
            (
                "connections",
                json!({"name":"probe","kind":"github","base_url":base}),
            ),
            (
                "providers",
                json!({"name":"probe","kind":"openai","default_model":"m","base_url":base}),
            ),
            (
                "notification-providers",
                json!({"name":"probe","kind":"webhook","url":base}),
            ),
        ] {
            assert_eq!(
                request(
                    &app,
                    "POST",
                    &format!("/api/{kind}"),
                    Some(body),
                    Some(&cookie)
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
    }
    for kind in [
        "connections",
        "providers",
        "notification_providers",
        "automations",
    ] {
        assert!(diffrook::db::objects(&state.db, kind)
            .await
            .unwrap()
            .is_empty());
    }
    let (_, saved, _) = request(
        &app,
        "POST",
        "/api/connections",
        Some(json!({"name":"valid","kind":"github"})),
        Some(&cookie),
    )
    .await;
    let mut forged = saved.clone();
    forged["is_admin"] = json!(true);
    assert_eq!(
        request(
            &app,
            "PUT",
            &format!("/api/connections/{}", saved["id"].as_str().unwrap()),
            Some(forged),
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let stored = diffrook::db::object(&state.db, "connections", saved["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(stored.get("is_admin").is_none());
    // Historical objects must also be blocked at the moment of use.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let legacy = json!({"id":"legacy","name":"legacy","kind":"openai","default_model":"test",
        "base_url":format!("http://{}", listener.local_addr().unwrap()), "created_at":"now","updated_at":"now"});
    diffrook::db::save_object(&state.db, "providers", &legacy)
        .await
        .unwrap();
    let (_, result, _) = request(
        &app,
        "POST",
        "/api/providers/legacy/test",
        Some(json!({})),
        Some(&cookie),
    )
    .await;
    assert_eq!(result["ok"], false);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}
