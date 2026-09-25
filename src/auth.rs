use crate::core::{clear_cookie, error, session_cookie, AppState};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::rngs::OsRng;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;

#[derive(Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
}
#[derive(Deserialize)]
pub struct SetupCredentials {
    username: String,
    password: String,
    setup_token: String,
}
fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
fn fresh_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}
use rand::RngCore;

pub async fn is_setup_required(state: &AppState) -> anyhow::Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await?;
    Ok(count == 0)
}
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> anyhow::Result<Option<String>> {
    let Some(raw) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|h| h.to_str().ok())
    else {
        return Ok(None);
    };
    let Some(token) = raw
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("diffrook_session="))
        .next()
    else {
        return Ok(None);
    };
    let row = sqlx::query("SELECT user_id,expires_at FROM sessions WHERE token_hash=?")
        .bind(hash_token(token))
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.get::<i64, _>("expires_at") <= chrono::Utc::now().timestamp() {
        sqlx::query("DELETE FROM sessions WHERE token_hash=?")
            .bind(hash_token(token))
            .execute(&state.db)
            .await?;
        return Ok(None);
    }
    Ok(Some(row.get("user_id")))
}
async fn create_session(state: &AppState, user_id: &str) -> anyhow::Result<String> {
    let token = fresh_token();
    let expires = chrono::Utc::now().timestamp() + 60 * 60 * 24 * 14;
    sqlx::query("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?,?,?)")
        .bind(hash_token(&token))
        .bind(user_id)
        .bind(expires)
        .execute(&state.db)
        .await?;
    Ok(token)
}
fn validate_credentials(username: &str, password: &str) -> Result<(), &'static str> {
    if username.len() < 2
        || username.len() > 64
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        return Err("Username must be 2-64 letters, digits, dots, dashes, or underscores");
    }
    if password.len() < 12 || password.len() > 1024 {
        return Err("Password must be between 12 and 1024 characters");
    }
    Ok(())
}
pub async fn setup(State(state): State<AppState>, Json(input): Json<SetupCredentials>) -> Response {
    use subtle::ConstantTimeEq;
    if input.setup_token.len() != state.setup_token.len()
        || !bool::from(
            input
                .setup_token
                .as_bytes()
                .ct_eq(state.setup_token.as_bytes()),
        )
    {
        return error(StatusCode::UNAUTHORIZED, "Invalid setup token");
    }
    if let Err(e) = validate_credentials(&input.username, &input.password) {
        return error(StatusCode::BAD_REQUEST, e);
    }
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let count: i64 = match sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(c) => c,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if count > 0 {
        return error(StatusCode::CONFLICT, "Initial setup is already complete");
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = match Argon2::default().hash_password(input.password.as_bytes(), &salt) {
        Ok(h) => h.to_string(),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let id = uuid::Uuid::new_v4().to_string();
    if let Err(e) =
        sqlx::query("INSERT INTO users(id,username,password_hash,created_at) VALUES(?,?,?,?)")
            .bind(&id)
            .bind(input.username.trim())
            .bind(hash)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await
    {
        return error(StatusCode::CONFLICT, e);
    }
    if let Err(e) = tx.commit().await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    match create_session(&state, &id).await {
        Ok(token) => (
            [(
                axum::http::header::SET_COOKIE,
                session_cookie(&token, 60 * 60 * 24 * 14),
            )],
            Json(json!({"ok":true})),
        )
            .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
pub async fn login(State(state): State<AppState>, Json(input): Json<Credentials>) -> Response {
    if input.username.len() > 64 || input.password.len() > 1024 || input.password.is_empty() {
        return error(StatusCode::UNAUTHORIZED, "Invalid username or password");
    }
    let key = input.username.trim().to_lowercase();
    let now = chrono::Utc::now().timestamp();
    {
        let mut failures = state.login_failures.lock().await;
        if let Some((count, until)) = failures.get(&key) {
            if *until > now && *count >= 10 {
                return error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many login attempts. Try again in 15 minutes",
                );
            }
        }
        failures.retain(|_, (_, until)| *until > now);
    }
    let row =
        match sqlx::query("SELECT id,password_hash FROM users WHERE username=? COLLATE NOCASE")
            .bind(input.username.trim())
            .fetch_optional(&state.db)
            .await
        {
            Ok(r) => r,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        };
    let valid_user = if let Some(row) = &row {
        let hash: String = row.get("password_hash");
        PasswordHash::new(&hash).ok().is_some_and(|parsed| {
            Argon2::default()
                .verify_password(input.password.as_bytes(), &parsed)
                .is_ok()
        })
    } else {
        false
    };
    if !valid_user {
        let mut failures = state.login_failures.lock().await;
        let entry = failures.entry(key).or_insert((0, now + 900));
        if entry.1 <= now {
            *entry = (0, now + 900)
        }
        entry.0 = entry.0.saturating_add(1);
        return error(StatusCode::UNAUTHORIZED, "Invalid username or password");
    }
    state.login_failures.lock().await.remove(&key);
    let row = row.unwrap();
    match create_session(&state, &row.get::<String, _>("id")).await {
        Ok(token) => (
            [(
                axum::http::header::SET_COOKIE,
                session_cookie(&token, 60 * 60 * 24 * 14),
            )],
            Json(json!({"ok":true})),
        )
            .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .find_map(|p| p.trim().strip_prefix("diffrook_session="))
        })
    {
        let _ = sqlx::query("DELETE FROM sessions WHERE token_hash=?")
            .bind(hash_token(token))
            .execute(&state.db)
            .await;
    }
    (
        [(axum::http::header::SET_COOKIE, clear_cookie())],
        Json(json!({"ok":true})),
    )
        .into_response()
}
