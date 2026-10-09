use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::Context;
use axum::{http::header, response::IntoResponse, Json};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub key: Arc<[u8; 32]>,
    pub client: reqwest::Client,
    pub setup_token: Arc<String>,
    pub data_dir: PathBuf,
    pub cancellations: Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
    >,
    pub login_failures: Arc<tokio::sync::Mutex<std::collections::HashMap<String, (u32, i64)>>>,
    pub updates: Arc<tokio::sync::RwLock<Value>>,
    pub restart: tokio::sync::watch::Sender<bool>,
    pub update_lock: Arc<tokio::sync::Mutex<()>>,
    pub security: Arc<crate::security::SecurityConfig>,
    pub rate_limiter: Arc<tokio::sync::Mutex<crate::security::RateLimiter>>,
    pub password_slots: Arc<tokio::sync::Semaphore>,
    pub dummy_password_hash: Arc<String>,
    pub oidc_metadata: Arc<
        tokio::sync::Mutex<
            Option<(
                std::time::Instant,
                openidconnect::core::CoreProviderMetadata,
            )>,
        >,
    >,
}

impl AppState {
    pub async fn new(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::with_security(data_dir, crate::security::SecurityConfig::from_env()?).await
    }

    pub async fn with_security(
        data_dir: impl AsRef<Path>,
        security: crate::security::SecurityConfig,
    ) -> anyhow::Result<Self> {
        let dummy_password_hash =
            tokio::task::spawn_blocking(|| crate::auth::hash_password(&crate::auth::fresh_token()))
                .await??;
        let data_dir = data_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&data_dir).await?;
        let key_path = data_dir.join("encryption.key");
        let key = if tokio::fs::try_exists(&key_path).await? {
            let bytes = tokio::fs::read(&key_path).await?;
            anyhow::ensure!(bytes.len() == 32, "invalid encryption.key length");
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            key
        } else {
            anyhow::ensure!(
                !tokio::fs::try_exists(data_dir.join("diffrook.sqlite")).await?,
                "encryption.key is missing from an existing database; restore the matching key from backup"
            );
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            tokio::fs::write(&key_path, key).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                    .await?;
            }
            key
        };
        let db_path = data_dir.join("diffrook.sqlite");
        let db = crate::db::open(db_path.to_str().context("database path is not UTF-8")?).await?;
        let setup_token = if !security.local_login {
            String::new()
        } else if let Some(token) = std::env::var("DIFFROOK_SETUP_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())
        {
            token
        } else {
            let path = data_dir.join("setup_token");
            if tokio::fs::try_exists(&path).await? {
                tokio::fs::read_to_string(path).await?
            } else {
                let token = format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                );
                tokio::fs::write(&path, &token).await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .await?;
                }
                tracing::warn!("Initial Diffrook setup token: {token}");
                token
            }
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let (restart, _) = tokio::sync::watch::channel(false);
        Ok(Self {
            db,
            key: Arc::new(key),
            client,
            setup_token: Arc::new(setup_token.trim().to_owned()),
            data_dir,
            cancellations: Arc::new(tokio::sync::Mutex::new(Default::default())),
            login_failures: Arc::new(tokio::sync::Mutex::new(Default::default())),
            updates: Arc::new(tokio::sync::RwLock::new(
                json!({"current_version":env!("CARGO_PKG_VERSION"),"releases":[],"checked_at":null,"error":null}),
            )),
            restart,
            update_lock: Arc::new(tokio::sync::Mutex::new(())),
            security: Arc::new(security),
            rate_limiter: Arc::new(tokio::sync::Mutex::new(Default::default())),
            password_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            dummy_password_hash: Arc::new(dummy_password_hash),
            oidc_metadata: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub fn encrypt(&self, plain: &str) -> anyhow::Result<String> {
        if plain.is_empty() {
            return Ok(String::new());
        }
        if plain.starts_with("enc:") && self.decrypt(plain).is_ok() {
            return Ok(plain.to_owned());
        }
        let cipher = Aes256Gcm::new_from_slice(self.key.as_ref())?;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut data = nonce.to_vec();
        data.extend(
            cipher
                .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
                .map_err(|_| anyhow::anyhow!("encryption failed"))?,
        );
        Ok(format!("enc:{}", STANDARD.encode(data)))
    }
    pub fn decrypt(&self, stored: &str) -> anyhow::Result<String> {
        let Some(encoded) = stored.strip_prefix("enc:") else {
            return Ok(stored.to_owned());
        };
        let data = STANDARD.decode(encoded)?;
        anyhow::ensure!(data.len() >= 28, "invalid encrypted value");
        let cipher = Aes256Gcm::new_from_slice(self.key.as_ref())?;
        let bytes = cipher
            .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
            .map_err(|_| anyhow::anyhow!("decryption failed"))?;
        Ok(String::from_utf8(bytes)?)
    }
    pub fn protect(&self, kind: &str, value: &mut Value) -> anyhow::Result<()> {
        for field in match kind {
            "connections" => vec!["token", "webhook_secret"],
            "providers" => vec!["api_key"],
            "notification_providers" => vec!["url"],
            _ => vec![],
        } {
            if let Some(s) = value[field].as_str() {
                value[field] = serde_json::Value::String(self.encrypt(s)?);
            }
        }
        if kind == "automations" {
            if let Some(items) = value["notifications"].as_array_mut() {
                for item in items {
                    if let Some(s) = item["url"].as_str() {
                        item["url"] = serde_json::Value::String(self.encrypt(s)?);
                    }
                }
            }
        }
        Ok(())
    }
    pub fn reveal(&self, kind: &str, value: &mut Value) -> anyhow::Result<()> {
        for field in match kind {
            "connections" => vec!["token", "webhook_secret"],
            "providers" => vec!["api_key"],
            "notification_providers" => vec!["url"],
            _ => vec![],
        } {
            if let Some(s) = value[field].as_str() {
                value[field] = serde_json::Value::String(self.decrypt(s)?);
            }
        }
        if kind == "automations" {
            if let Some(items) = value["notifications"].as_array_mut() {
                for item in items {
                    if let Some(s) = item["url"].as_str() {
                        item["url"] = serde_json::Value::String(self.decrypt(s)?);
                    }
                }
            }
        }
        Ok(())
    }
    pub fn mask(&self, kind: &str, value: &mut Value) -> anyhow::Result<()> {
        for (field, flag) in match kind {
            "connections" => vec![
                ("token", "has_token"),
                ("webhook_secret", "has_webhook_secret"),
            ],
            "providers" => vec![("api_key", "has_api_key")],
            "notification_providers" => vec![("url", "has_url")],
            _ => vec![],
        } {
            let s = value[field].as_str().unwrap_or("");
            let has = !s.is_empty();
            value[flag] = json!(has);
            value[field] = json!("");
        }
        if kind == "automations" {
            if let Some(items) = value["notifications"].as_array_mut() {
                for item in items {
                    let s = item["url"].as_str().unwrap_or("");
                    item["has_url"] = json!(!s.is_empty());
                    item["url"] = json!("");
                }
            }
        }
        Ok(())
    }
}

pub fn error(status: axum::http::StatusCode, msg: impl ToString) -> axum::response::Response {
    let message = if status.is_server_error() {
        tracing::error!(%status, "API request failed");
        "Internal server error".to_owned()
    } else {
        msg.to_string()
    };
    (status, Json(json!({"error": message}))).into_response()
}
pub fn request_origin_ok(headers: &axum::http::HeaderMap) -> bool {
    if headers
        .get("x-diffrook-request")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return false;
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        return headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|host| {
                url::Url::parse(origin).ok().is_some_and(|u| {
                    let host_url = url::Url::parse(&format!("{}://{}", u.scheme(), host)).ok();
                    host_url.is_some_and(|h| {
                        h.host_str() == u.host_str()
                            && h.port_or_known_default() == u.port_or_known_default()
                    })
                })
            });
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn secrets_encrypt_round_trip_and_mask_without_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path()).await.unwrap();
        let mut connection = json!({"token":"api-secret","webhook_secret":"hook-secret"});
        state.protect("connections", &mut connection).unwrap();
        let stored = connection.to_string();
        assert!(!stored.contains("api-secret"));
        assert!(!stored.contains("hook-secret"));
        let mut shown = connection.clone();
        state.mask("connections", &mut shown).unwrap();
        assert_eq!(shown["token"], "");
        assert_eq!(shown["has_token"], true);
        assert_eq!(shown["webhook_secret"], "");
        assert_eq!(shown["has_webhook_secret"], true);
        state.reveal("connections", &mut connection).unwrap();
        assert_eq!(connection["token"], "api-secret");
        assert_eq!(connection["webhook_secret"], "hook-secret");
    }
    #[tokio::test]
    async fn notification_provider_url_is_encrypted_and_masked() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path()).await.unwrap();
        let mut provider = json!({"url":"https://hooks.slack.com/services/test-secret"});
        state
            .protect("notification_providers", &mut provider)
            .unwrap();
        assert!(!provider.to_string().contains("test-secret"));
        let mut shown = provider.clone();
        state.mask("notification_providers", &mut shown).unwrap();
        assert_eq!(shown["url"], "");
        assert_eq!(shown["has_url"], true);
        state
            .reveal("notification_providers", &mut provider)
            .unwrap();
        assert_eq!(
            provider["url"],
            "https://hooks.slack.com/services/test-secret"
        );
    }
    #[test]
    fn origin_check_requires_custom_header_and_matches_host_and_port() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("host", "example.test:8443".parse().unwrap());
        h.insert("x-diffrook-request", "1".parse().unwrap());
        assert!(request_origin_ok(&h));
        h.insert(
            axum::http::header::ORIGIN,
            "https://example.test:8443".parse().unwrap(),
        );
        assert!(request_origin_ok(&h));
        h.insert(
            axum::http::header::ORIGIN,
            "https://example.test".parse().unwrap(),
        );
        assert!(!request_origin_ok(&h));
        h.insert(
            axum::http::header::ORIGIN,
            "https://attacker.test:8443".parse().unwrap(),
        );
        assert!(!request_origin_ok(&h));
        h.remove("x-diffrook-request");
        assert!(!request_origin_ok(&h));
    }
}
