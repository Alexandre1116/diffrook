use crate::{
    auth,
    core::{error, AppState},
    security::OidcConfig,
};
use anyhow::{ensure, Context};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use openidconnect::{
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpRequest,
    HttpResponse, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};
use serde::Deserialize;
use sqlx::Row;
use std::{
    io,
    time::{Duration, Instant},
};

const LOGIN_SECONDS: i64 = 300;
const MAX_PENDING: i64 = 1000;

async fn http(
    client: reqwest::Client,
    config: OidcConfig,
    request: HttpRequest,
) -> Result<HttpResponse, io::Error> {
    config
        .validate_endpoint(&request.uri().to_string())
        .map_err(|_| io::Error::other("Invalid OIDC endpoint"))?;
    let response = client
        .request(request.method().clone(), request.uri().to_string())
        .headers(request.headers().clone())
        .body(request.body().clone())
        .send()
        .await
        .map_err(|_| io::Error::other("OIDC endpoint request failed"))?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = crate::network::read_body(response, 512 * 1024)
        .await
        .map_err(|_| io::Error::other("OIDC response is unavailable or too large"))?;
    let mut response = HttpResponse::new(bytes);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

async fn metadata(state: &AppState, config: &OidcConfig) -> anyhow::Result<CoreProviderMetadata> {
    // Serialize discovery and cache it briefly, so unauthenticated traffic cannot fan out to the IdP.
    let mut cached = tokio::time::timeout(Duration::from_secs(10), state.oidc_metadata.lock())
        .await
        .context("OIDC discovery is busy")?;
    if let Some((when, metadata)) = cached.as_ref() {
        if when.elapsed() < Duration::from_secs(300) {
            return Ok(metadata.clone());
        }
    }
    let transport_client = state.client.clone();
    let transport_config = config.clone();
    let transport =
        move |request| http(transport_client.clone(), transport_config.clone(), request);
    let discovered = tokio::time::timeout(
        Duration::from_secs(10),
        CoreProviderMetadata::discover_async(IssuerUrl::new(config.issuer.clone())?, &transport),
    )
    .await
    .context("OIDC discovery timed out")?
    .map_err(|_| anyhow::anyhow!("OIDC discovery failed"))?;
    config.validate_endpoint(discovered.authorization_endpoint().as_str())?;
    config.validate_endpoint(
        discovered
            .token_endpoint()
            .context("OIDC token endpoint is missing")?
            .as_str(),
    )?;
    *cached = Some((Instant::now(), discovered.clone()));
    Ok(discovered)
}

fn redirect_uri(state: &AppState) -> anyhow::Result<RedirectUrl> {
    let public = state
        .security
        .public_url
        .as_ref()
        .context("Public URL is missing")?;
    RedirectUrl::new(public.join("api/auth/oidc/callback")?.to_string()).map_err(Into::into)
}

pub async fn start(State(state): State<AppState>) -> Response {
    let Some(config) = &state.security.oidc else {
        return error(StatusCode::NOT_FOUND, "SSO is not configured");
    };
    match state.license.can_start_sso(&state.db).await {
        Ok(true) => (),
        Ok(false) => return error(StatusCode::FORBIDDEN, crate::licensing::SsoPlanRequired),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
    match begin(&state, config).await {
        Ok(response) => response,
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SSO is temporarily unavailable",
        ),
    }
}

async fn begin(state: &AppState, config: &OidcConfig) -> anyhow::Result<Response> {
    if !state.license.can_start_sso(&state.db).await? {
        anyhow::bail!(crate::licensing::SsoPlanRequired);
    }
    let client = CoreClient::from_provider_metadata(
        metadata(state, config).await?,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
    )
    .set_redirect_uri(redirect_uri(state)?);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".to_owned()))
        .set_pkce_challenge(challenge)
        .url();
    let binding = auth::fresh_token();
    let now = chrono::Utc::now().timestamp();
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM oidc_logins WHERE expires_at<=?")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oidc_logins")
        .fetch_one(&mut *tx)
        .await?;
    ensure!(pending < MAX_PENDING, "Too many pending logins");
    sqlx::query("INSERT INTO oidc_logins(state_hash,binding_hash,nonce,verifier,expires_at) VALUES(?,?,?,?,?)")
        .bind(auth::hash_token(csrf.secret())).bind(auth::hash_token(&binding))
        .bind(state.encrypt(nonce.secret())?).bind(state.encrypt(verifier.secret())?).bind(now + LOGIN_SECONDS)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        [(
            header::SET_COOKIE,
            state.security.cookie(
                state.security.oidc_cookie_name(),
                &binding,
                LOGIN_SECONDS,
                true,
            ),
        )],
        Redirect::to(url.as_str()),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn binding<'a>(state: &AppState, headers: &'a HeaderMap) -> Option<&'a str> {
    let prefix = format!("{}=", state.security.oidc_cookie_name());
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .find_map(|s| s.trim().strip_prefix(&prefix))
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    let result = finish(&state, &headers, query).await;
    let mut response = match result {
        Ok(token) => {
            let mut response = Redirect::to("/").into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                state.security.cookie(
                    state.security.session_cookie_name(),
                    &token,
                    state.security.session_seconds,
                    false,
                ),
            );
            response
        }
        Err(e) => {
            tracing::warn!("SSO login rejected");
            Redirect::to(if e.is::<crate::licensing::UserLimitReached>() {
                "/?sso_error=user_limit"
            } else if e.is::<crate::licensing::SsoPlanRequired>() {
                "/?sso_error=plan_required"
            } else {
                "/?sso_error=failed"
            })
            .into_response()
        }
    };
    response.headers_mut().append(
        header::SET_COOKIE,
        state
            .security
            .cookie(state.security.oidc_cookie_name(), "", 0, true),
    );
    response
}

async fn finish(state: &AppState, headers: &HeaderMap, query: Callback) -> anyhow::Result<String> {
    let config = state
        .security
        .oidc
        .as_ref()
        .context("SSO is not configured")?;
    let browser = binding(state, headers)
        .filter(|b| b.len() <= 128)
        .context("Missing login cookie")?;
    let csrf = query
        .state
        .as_deref()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .context("Missing login state")?;
    // Atomic consumption prevents replay, including simultaneous callback deliveries.
    let pending = sqlx::query("DELETE FROM oidc_logins WHERE state_hash=? AND binding_hash=? AND expires_at>? RETURNING nonce,verifier")
        .bind(auth::hash_token(csrf)).bind(auth::hash_token(browser)).bind(chrono::Utc::now().timestamp()).fetch_optional(&state.db).await?
        .context("Invalid or expired login state")?;
    ensure!(query.error.is_none(), "Identity provider rejected login");
    let code = query
        .code
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .context("Missing authorization code")?;
    let nonce = Nonce::new(state.decrypt(&pending.get::<String, _>("nonce"))?);
    let verifier = PkceCodeVerifier::new(state.decrypt(&pending.get::<String, _>("verifier"))?);
    let client = CoreClient::from_provider_metadata(
        metadata(state, config).await?,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
    )
    .set_redirect_uri(redirect_uri(state)?);
    let transport_client = state.client.clone();
    let transport_config = config.clone();
    let transport =
        move |request| http(transport_client.clone(), transport_config.clone(), request);
    let token = client
        .exchange_code(AuthorizationCode::new(code))?
        .set_pkce_verifier(verifier)
        .request_async(&transport)
        .await
        .map_err(|_| anyhow::anyhow!("OIDC code exchange failed"))?;
    let id_token = token.id_token().context("Missing ID token")?;
    let verifier = client.id_token_verifier();
    // The library validates the signature, algorithm, issuer, audience, expiry and nonce.
    let claims = id_token
        .claims(&verifier, &nonce)
        .map_err(|_| anyhow::anyhow!("Invalid ID token"))?;
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            token.access_token(),
            id_token.signing_alg()?,
            id_token.signing_key(&verifier)?,
        )?;
        ensure!(&actual == expected, "Invalid access token hash");
    }
    ensure!(
        claims.issue_time() <= chrono::Utc::now() + chrono::Duration::seconds(60),
        "ID token issued in the future"
    );
    let subject = claims.subject().as_str();
    ensure!(
        !subject.is_empty() && subject.len() <= 1024,
        "Invalid subject"
    );
    let email = claims.email().map(|e| e.as_str());
    let verified = claims.email_verified() == Some(true);
    ensure!(
        config.allows(subject, email, verified),
        "Identity is not authorized"
    );
    let user_id = provision(state, config, subject, email, verified).await?;
    auth::create_session(state, &user_id, "sso").await
}

async fn provision(
    state: &AppState,
    config: &OidcConfig,
    subject: &str,
    email: Option<&str>,
    verified: bool,
) -> anyhow::Result<String> {
    let mut tx = state.db.begin().await?;
    // Never link a local account by username or email. An identity is the exact issuer + subject.
    let existing: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM sso_identities WHERE issuer=? AND subject=?")
            .bind(&config.issuer)
            .bind(subject)
            .fetch_optional(&mut *tx)
            .await?;
    let id = if let Some(id) = existing {
        id
    } else {
        if !state.license.allows_sso() {
            anyhow::bail!(crate::licensing::SsoPlanRequired);
        }
        let uuid = uuid::Uuid::new_v4();
        let id = uuid.to_string();
        let limit = state.license.limits().users.map(i64::from);
        let inserted = sqlx::query("INSERT INTO users(id,username,password_hash,created_at) SELECT ?,?,'',? WHERE (? IS NULL OR (SELECT COUNT(*) FROM users) < ?)")
            .bind(&id)
            .bind(format!("sso.{}", uuid.simple()))
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(limit).bind(limit)
            .execute(&mut *tx)
            .await?;
        if inserted.rows_affected() != 1 {
            anyhow::bail!(crate::licensing::UserLimitReached);
        }
        sqlx::query("INSERT INTO sso_identities(issuer,subject,user_id) VALUES(?,?,?)")
            .bind(&config.issuer)
            .bind(subject)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        id
    };
    sqlx::query("UPDATE sso_identities SET email=?,email_verified=? WHERE issuer=? AND subject=?")
        .bind(email)
        .bind(verified)
        .bind(&config.issuer)
        .bind(subject)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityConfig;
    use axum::{
        body::Body,
        extract::Form,
        http::Request,
        routing::{get, post},
        Json, Router,
    };
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use openidconnect::{
        core::{
            CoreEdDsaPrivateSigningKey, CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet,
            CoreJwsSigningAlgorithm,
        },
        AccessToken, Audience, EmptyAdditionalClaims, EndUserEmail, JsonWebKeyId,
        PrivateSigningKey, StandardClaims, SubjectIdentifier,
    };
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    use tower::ServiceExt;

    #[derive(Clone)]
    struct MockIdp {
        issuer: String,
        nonce: Arc<tokio::sync::Mutex<String>>,
        challenge: Arc<tokio::sync::Mutex<String>>,
        mode: Arc<tokio::sync::Mutex<String>>,
        exchanges: Arc<AtomicUsize>,
    }
    fn key() -> CoreEdDsaPrivateSigningKey {
        CoreEdDsaPrivateSigningKey::from_ed25519_pem(
            include_str!("../tests/fixtures/oidc-test-key.pem"),
            Some(JsonWebKeyId::new("test-key".into())),
        )
        .unwrap()
    }
    async fn discovery(State(mock): State<MockIdp>) -> Json<Value> {
        Json(
            json!({"issuer":mock.issuer,"authorization_endpoint":format!("{}/authorize",mock.issuer),"token_endpoint":format!("{}/token",mock.issuer),"jwks_uri":format!("{}/jwks",mock.issuer),"response_types_supported":["code"],"subject_types_supported":["public"],"id_token_signing_alg_values_supported":["EdDSA"],"token_endpoint_auth_methods_supported":["client_secret_basic"],"code_challenge_methods_supported":["S256"]}),
        )
    }
    async fn jwks() -> Json<Value> {
        Json(
            serde_json::to_value(CoreJsonWebKeySet::new(vec![key().as_verification_key()]))
                .unwrap(),
        )
    }
    async fn token(
        State(mock): State<MockIdp>,
        headers: HeaderMap,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<Value> {
        mock.exchanges.fetch_add(1, Ordering::SeqCst);
        assert_eq!(form.get("grant_type").unwrap(), "authorization_code");
        assert_eq!(
            form.get("redirect_uri").unwrap(),
            "https://diffrook.example.test/api/auth/oidc/callback"
        );
        assert!(headers
            .get(header::AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("Basic "));
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(
                form.get("code_verifier").unwrap().as_bytes()
            )),
            *mock.challenge.lock().await
        );
        let mode = mock.mode.lock().await.clone();
        let now = chrono::Utc::now();
        let standard = StandardClaims::new(SubjectIdentifier::new(
            if mode == "subject-only" {
                "trusted-subject"
            } else {
                "employee-123"
            }
            .into(),
        ))
        .set_email(if mode == "subject-only" {
            None
        } else {
            Some(EndUserEmail::new(
                if mode == "unauthorized" {
                    "outsider@example.test"
                } else {
                    "admin@example.test"
                }
                .into(),
            ))
        })
        .set_email_verified(Some(mode != "unverified"));
        let claims = CoreIdTokenClaims::new(
            IssuerUrl::new(if mode == "issuer" {
                "https://wrong-issuer.example.test".into()
            } else {
                mock.issuer.clone()
            })
            .unwrap(),
            vec![Audience::new(
                if mode == "audience" {
                    "other-client"
                } else {
                    "test-client"
                }
                .into(),
            )],
            now + chrono::Duration::seconds(if mode == "expired" { -300 } else { 300 }),
            now + chrono::Duration::seconds(if mode == "future" { 3600 } else { 0 }),
            standard,
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(Nonce::new(if mode == "nonce" {
            "wrong-nonce".into()
        } else {
            mock.nonce.lock().await.clone()
        })));
        let access = AccessToken::new("mock-access-token".into());
        let id = CoreIdToken::new(
            claims,
            &key(),
            CoreJwsSigningAlgorithm::EdDsa,
            Some(&access),
            None,
        )
        .unwrap();
        let mut serialized = id.to_string();
        if mode == "signature" {
            let at = serialized.rfind('.').unwrap() + 1;
            let mut signature = URL_SAFE_NO_PAD.decode(&serialized[at..]).unwrap();
            signature[0] ^= 1;
            serialized.replace_range(at.., &URL_SAFE_NO_PAD.encode(signature));
        }
        Json(
            json!({"access_token":access.secret(),"token_type":"Bearer","id_token":serialized,"expires_in":300}),
        )
    }
    struct Fixture {
        state: AppState,
        mock: MockIdp,
        task: tokio::task::JoinHandle<()>,
        _dir: tempfile::TempDir,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn fixture(subjects: Vec<String>) -> Fixture {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock = MockIdp {
            issuer: format!("http://{}", listener.local_addr().unwrap()),
            nonce: Default::default(),
            challenge: Default::default(),
            mode: Default::default(),
            exchanges: Default::default(),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(mock.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let security = SecurityConfig {
            public_url: Some(url::Url::parse("https://diffrook.example.test").unwrap()),
            secure_cookies: true,
            local_login: false,
            oidc: Some(OidcConfig {
                issuer: mock.issuer.clone(),
                client_id: "test-client".into(),
                client_secret: "mock-client-secret".into(),
                allowed_subjects: subjects,
                allowed_emails: vec!["admin@example.test".into()],
                allow_test_http: true,
            }),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::with_security(dir.path(), security).await.unwrap();
        state.license = Arc::new(crate::licensing::License::subscription_for_tests(
            state.license.installation_id,
            "freelancer",
            1,
            0,
        ));
        Fixture {
            state,
            mock,
            task,
            _dir: dir,
        }
    }
    async fn begin_login(fixture: &Fixture) -> (String, HeaderMap) {
        let response = crate::routes::router(fixture.state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/auth/oidc/start")
                    .header("host", "diffrook.example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = url::Url::parse(
            response
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let values: HashMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(values["code_challenge_method"], "S256");
        assert_eq!(
            values["redirect_uri"],
            "https://diffrook.example.test/api/auth/oidc/callback"
        );
        *fixture.mock.nonce.lock().await = values["nonce"].clone();
        *fixture.mock.challenge.lock().await = values["code_challenge"].clone();
        let raw = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(raw.starts_with("__Host-diffrook_oidc="));
        assert!(raw.contains("HttpOnly") && raw.contains("SameSite=Lax") && raw.contains("Secure"));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            raw.split(';').next().unwrap().parse().unwrap(),
        );
        (values["state"].clone(), headers)
    }
    fn query(state: String) -> Callback {
        Callback {
            state: Some(state),
            code: Some("mock-authorization-code".into()),
            error: None,
        }
    }

    #[tokio::test]
    async fn signed_tokens_require_correct_signature_issuer_audience_nonce_expiry_and_authorization(
    ) {
        let fixture = fixture(vec![]).await;
        for mode in [
            "signature",
            "issuer",
            "audience",
            "nonce",
            "expired",
            "future",
            "unauthorized",
            "unverified",
        ] {
            *fixture.mock.mode.lock().await = mode.into();
            let (state, headers) = begin_login(&fixture).await;
            assert!(
                finish(&fixture.state, &headers, query(state))
                    .await
                    .is_err(),
                "accepted invalid {mode} token"
            );
        }
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&fixture.state.db)
            .await
            .unwrap();
        assert_eq!(users, 0, "rejected logins must not provision users");
        *fixture.mock.mode.lock().await = "valid".into();
        let (state, headers) = begin_login(&fixture).await;
        let token = finish(&fixture.state, &headers, query(state.clone()))
            .await
            .unwrap();
        assert!(
            finish(&fixture.state, &headers, query(state))
                .await
                .is_err(),
            "callback was replayed"
        );
        let mut session = HeaderMap::new();
        session.insert(
            header::COOKIE,
            format!("__Host-diffrook_session={token}").parse().unwrap(),
        );
        assert!(auth::authenticate(&fixture.state, &session)
            .await
            .unwrap()
            .is_some());
        assert_eq!(fixture.mock.exchanges.load(Ordering::SeqCst), 9);
    }

    #[tokio::test]
    async fn callback_is_bound_to_browser_expires_and_can_only_be_consumed_once() {
        let fixture = fixture(vec![]).await;
        let (state, headers) = begin_login(&fixture).await;
        assert!(
            finish(&fixture.state, &HeaderMap::new(), query(state.clone()))
                .await
                .is_err()
        );
        let mut other = HeaderMap::new();
        other.insert(
            header::COOKIE,
            "__Host-diffrook_oidc=wrong-browser".parse().unwrap(),
        );
        assert!(finish(&fixture.state, &other, query(state.clone()))
            .await
            .is_err());
        assert!(
            finish(&fixture.state, &headers, query("wrong-state".into()))
                .await
                .is_err()
        );
        assert_eq!(fixture.mock.exchanges.load(Ordering::SeqCst), 0);
        sqlx::query("UPDATE oidc_logins SET expires_at=0")
            .execute(&fixture.state.db)
            .await
            .unwrap();
        assert!(finish(&fixture.state, &headers, query(state))
            .await
            .is_err());
        let (state, headers) = begin_login(&fixture).await;
        let (a, b) = tokio::join!(
            finish(&fixture.state, &headers, query(state.clone())),
            finish(&fixture.state, &headers, query(state))
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(fixture.mock.exchanges.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn subjects_work_without_email_and_removed_permissions_revoke_existing_sessions() {
        let mut fixture = fixture(vec!["trusted-subject".into()]).await;
        *fixture.mock.mode.lock().await = "subject-only".into();
        let (state, headers) = begin_login(&fixture).await;
        let token = finish(&fixture.state, &headers, query(state))
            .await
            .unwrap();
        let mut session = HeaderMap::new();
        session.insert(
            header::COOKIE,
            format!("__Host-diffrook_session={token}").parse().unwrap(),
        );
        assert!(auth::authenticate(&fixture.state, &session)
            .await
            .unwrap()
            .is_some());
        let mut config = fixture.state.security.as_ref().clone();
        config.oidc.as_mut().unwrap().allowed_subjects.clear();
        fixture.state.security = Arc::new(config);
        assert!(auth::authenticate(&fixture.state, &session)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn provisioning_uses_subject_identity_without_linking_existing_local_accounts() {
        let mut fixture = fixture(vec![]).await;
        fixture.state.license = Arc::new(crate::licensing::License::business_for_tests(
            fixture.state.license.installation_id,
        ));
        sqlx::query("INSERT INTO users(id,username,password_hash,created_at) VALUES('local-admin','admin','unused','now')").execute(&fixture.state.db).await.unwrap();
        let config = fixture.state.security.oidc.as_ref().unwrap();
        let a = provision(
            &fixture.state,
            config,
            "employee-123",
            Some("admin@example.test"),
            true,
        )
        .await
        .unwrap();
        let b = provision(
            &fixture.state,
            config,
            "employee-123",
            Some("renamed@example.test"),
            true,
        )
        .await
        .unwrap();
        assert_eq!(a, b);
        assert_ne!(a, "local-admin");
        let password: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=?")
            .bind(a)
            .fetch_one(&fixture.state.db)
            .await
            .unwrap();
        assert!(password.is_empty());
    }

    #[tokio::test]
    async fn freelancer_limits_new_sso_users_but_allows_existing_identity_to_sign_in() {
        let fixture = fixture(vec![]).await;
        let config = fixture.state.security.oidc.as_ref().unwrap();
        let user = provision(&fixture.state, config, "personal", None, false)
            .await
            .unwrap();
        assert_eq!(
            provision(&fixture.state, config, "personal", None, false)
                .await
                .unwrap(),
            user
        );
        let rejected = provision(&fixture.state, config, "second", None, false)
            .await
            .unwrap_err();
        assert!(rejected.is::<crate::licensing::UserLimitReached>());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&fixture.state.db)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn local_account_uses_the_freelancer_user_slot_and_sso_cannot_bypass_it() {
        let fixture = fixture(vec![]).await;
        sqlx::query("INSERT INTO users(id,username,password_hash,created_at) VALUES('local','personal','unused','now')").execute(&fixture.state.db).await.unwrap();
        let error = provision(
            &fixture.state,
            fixture.state.security.oidc.as_ref().unwrap(),
            "personal-subject",
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(error.is::<crate::licensing::UserLimitReached>());
    }

    #[tokio::test]
    async fn concurrent_sso_provisioning_cannot_create_multiple_freelancer_users() {
        let fixture = fixture(vec![]).await;
        let mut pending = tokio::task::JoinSet::new();
        for n in 0..8 {
            let state = fixture.state.clone();
            pending.spawn(async move {
                provision(
                    &state,
                    state.security.oidc.as_ref().unwrap(),
                    &format!("user-{n}"),
                    None,
                    false,
                )
                .await
            });
        }
        let mut created = 0;
        while let Some(result) = pending.join_next().await {
            if result.unwrap().is_ok() {
                created += 1;
            }
        }
        assert_eq!(created, 1);
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&fixture.state.db)
            .await
            .unwrap();
        let identities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sso_identities")
            .fetch_one(&fixture.state.db)
            .await
            .unwrap();
        assert_eq!(users, 1);
        assert_eq!(identities, 1);
    }

    #[tokio::test]
    async fn individual_blocks_new_sso_but_retains_verified_existing_identity_access_on_downgrade()
    {
        let mut fixture = fixture(vec![]).await;
        let paid = fixture.state.license.clone();
        fixture.state.license = Arc::new(crate::licensing::License::individual_for_tests(
            paid.installation_id,
        ));
        assert!(!fixture
            .state
            .license
            .can_start_sso(&fixture.state.db)
            .await
            .unwrap());
        let response = start(State(fixture.state.clone())).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(fixture.mock.exchanges.load(Ordering::SeqCst), 0);
        let config = fixture.state.security.oidc.as_ref().unwrap();
        assert!(provision(&fixture.state, config, "new-user", None, false)
            .await
            .unwrap_err()
            .is::<crate::licensing::SsoPlanRequired>());
        fixture.state.license = paid;
        let (state, headers) = begin_login(&fixture).await;
        let token = finish(&fixture.state, &headers, query(state))
            .await
            .unwrap();
        fixture.state.license = Arc::new(crate::licensing::License::individual_for_tests(
            fixture.state.license.installation_id,
        ));
        assert!(fixture
            .state
            .license
            .can_start_sso(&fixture.state.db)
            .await
            .unwrap());
        let mut session = HeaderMap::new();
        session.insert(
            header::COOKIE,
            format!("__Host-diffrook_session={token}").parse().unwrap(),
        );
        assert!(auth::authenticate(&fixture.state, &session)
            .await
            .unwrap()
            .is_some());
        let (state, headers) = begin_login(&fixture).await;
        assert!(finish(&fixture.state, &headers, query(state)).await.is_ok());
        assert!(provision(
            &fixture.state,
            fixture.state.security.oidc.as_ref().unwrap(),
            "second-user",
            None,
            false
        )
        .await
        .unwrap_err()
        .is::<crate::licensing::SsoPlanRequired>());
    }

    #[tokio::test]
    async fn teams_and_enterprise_provision_only_their_purchased_user_allowances() {
        for (edition, users) in [("teams", 5), ("enterprise", 10)] {
            let mut fixture = fixture(vec![]).await;
            fixture.state.license = Arc::new(crate::licensing::License::subscription_for_tests(
                fixture.state.license.installation_id,
                edition,
                users,
                0,
            ));
            for n in 0..users {
                provision(
                    &fixture.state,
                    fixture.state.security.oidc.as_ref().unwrap(),
                    &format!("member-{n}"),
                    None,
                    false,
                )
                .await
                .unwrap();
            }
            let error = provision(
                &fixture.state,
                fixture.state.security.oidc.as_ref().unwrap(),
                "extra",
                None,
                false,
            )
            .await
            .unwrap_err();
            assert!(
                error.is::<crate::licensing::UserLimitReached>(),
                "{edition}"
            );
        }
    }
}
