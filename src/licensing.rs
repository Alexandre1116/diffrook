use anyhow::{ensure, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

pub const INDIVIDUAL_USERS: u32 = 1;
pub const INDIVIDUAL_AUTOMATIONS: u32 = 3;

#[derive(Debug)]
pub(crate) struct UserLimitReached;
impl std::fmt::Display for UserLimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("User limit reached for this plan")
    }
}
impl std::error::Error for UserLimitReached {}

#[derive(Clone, Copy, Serialize)]
pub struct Limits {
    pub users: Option<u32>,
    pub automations: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    version: u32,
    product: String,
    edition: String,
    license_id: uuid::Uuid,
    installation_id: uuid::Uuid,
    customer: String,
    issued_at: i64,
    expires_at: Option<i64>,
    max_users: Option<u32>,
    max_automations: Option<u32>,
}

#[derive(Clone)]
pub struct License {
    pub installation_id: uuid::Uuid,
    claims: Option<Claims>,
}

impl License {
    pub async fn load(db: &SqlitePool) -> anyhow::Result<Self> {
        sqlx::query("INSERT OR IGNORE INTO app_settings(key,value) VALUES('installation.id',?)")
            .bind(uuid::Uuid::new_v4().to_string())
            .execute(db)
            .await?;
        let id: String =
            sqlx::query_scalar("SELECT value FROM app_settings WHERE key='installation.id'")
                .fetch_one(db)
                .await?;
        let mut license = Self {
            installation_id: id.parse()?,
            claims: None,
        };
        if let Some(path) = std::env::var("DIFFROOK_LICENSE_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
        {
            ensure!(
                std::fs::metadata(&path)?.len() <= 32768,
                "License file is too large"
            );
            let bytes = std::fs::read(path)?;
            let raw = hex::decode(include_str!("license-public-key.hex").trim())?;
            let key = VerifyingKey::from_bytes(
                &raw.try_into()
                    .map_err(|_| anyhow::anyhow!("Invalid issuer public key"))?,
            )?;
            license.claims = Some(verify(&bytes, &key, license.installation_id)?);
        }
        Ok(license)
    }

    fn active(&self) -> Option<&Claims> {
        self.claims.as_ref().filter(|c| {
            c.expires_at
                .is_none_or(|t| t > chrono::Utc::now().timestamp())
        })
    }

    pub fn limits(&self) -> Limits {
        match self.active() {
            Some(c) => Limits {
                users: c.max_users,
                automations: c.max_automations,
            },
            None => Limits {
                users: Some(INDIVIDUAL_USERS),
                automations: Some(INDIVIDUAL_AUTOMATIONS),
            },
        }
    }

    pub async fn status(&self, db: &SqlitePool) -> anyhow::Result<serde_json::Value> {
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(db)
            .await?;
        let automations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM objects WHERE kind='automations'")
                .fetch_one(db)
                .await?;
        let limits = self.limits();
        let active = self.active();
        Ok(serde_json::json!({
            "edition": if active.is_some() { "business" } else { "individual" },
            "installation_id": self.installation_id,
            "license_status": if self.claims.is_none() { "not_installed" } else if active.is_some() { "active" } else { "expired" },
            "customer": self.claims.as_ref().map(|c| &c.customer),
            "expires_at": self.claims.as_ref().and_then(|c| c.expires_at),
            "limits": limits, "usage": {"users":users,"automations":automations},
            "can_create_user": limits.users.is_none_or(|max| users < i64::from(max)),
            "can_create_automation": limits.automations.is_none_or(|max| automations < i64::from(max)),
            "over_limit": limits.users.is_some_and(|max| users > i64::from(max)) || limits.automations.is_some_and(|max| automations > i64::from(max))
        }))
    }

    #[cfg(test)]
    pub(crate) fn business_for_tests(installation_id: uuid::Uuid) -> Self {
        Self {
            installation_id,
            claims: Some(Claims {
                version: 1,
                product: "diffrook".into(),
                edition: "business".into(),
                license_id: uuid::Uuid::new_v4(),
                installation_id,
                customer: "Unit tests".into(),
                issued_at: 0,
                expires_at: None,
                max_users: None,
                max_automations: None,
            }),
        }
    }
}

fn verify(bytes: &[u8], key: &VerifyingKey, installation: uuid::Uuid) -> anyhow::Result<Claims> {
    ensure!(bytes.len() <= 32768, "License file is too large");
    let envelope: Envelope = serde_json::from_slice(bytes).context("Invalid license format")?;
    let payload = URL_SAFE_NO_PAD.decode(envelope.payload)?;
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(envelope.signature)?)?;
    let message = [b"diffrook-license-v1\0".as_slice(), payload.as_slice()].concat();
    key.verify_strict(&message, &signature)
        .context("Invalid license signature")?;
    let claims: Claims = serde_json::from_slice(&payload).context("Invalid license claims")?;
    ensure!(
        claims.version == 1 && claims.product == "diffrook" && claims.edition == "business",
        "Unsupported license"
    );
    ensure!(
        claims.installation_id == installation,
        "License belongs to another installation"
    );
    ensure!(
        !claims.customer.trim().is_empty() && claims.customer.len() <= 512,
        "Invalid license customer"
    );
    ensure!(
        claims.issued_at >= 0 && claims.issued_at <= chrono::Utc::now().timestamp() + 60,
        "License issued in the future"
    );
    ensure!(
        claims.expires_at.is_none_or(|t| t > claims.issued_at),
        "Invalid license expiry"
    );
    ensure!(
        [claims.max_users, claims.max_automations]
            .into_iter()
            .all(|n| n.is_none_or(|v| (1..=2147483647).contains(&v))),
        "Invalid license limits"
    );
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn envelope(claims: &serde_json::Value, key: &SigningKey) -> Vec<u8> {
        let payload = serde_json::to_vec(claims).unwrap();
        let message = [b"diffrook-license-v1\0".as_slice(), payload.as_slice()].concat();
        serde_json::to_vec(&serde_json::json!({"payload":URL_SAFE_NO_PAD.encode(&payload),"signature":URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes())})).unwrap()
    }

    #[test]
    fn only_valid_signed_licenses_for_this_installation_enable_business() {
        let key = SigningKey::from_bytes(&[37; 32]); // disposable unit-test issuer, never the production key
        let id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now().timestamp();
        let claims = serde_json::json!({"version":1,"product":"diffrook","edition":"business","license_id":uuid::Uuid::new_v4(),"installation_id":id,"customer":"Test company","issued_at":now-60,"expires_at":now+3600,"max_users":5,"max_automations":10});
        let verified = verify(&envelope(&claims, &key), &key.verifying_key(), id).unwrap();
        let license = License {
            installation_id: id,
            claims: Some(verified),
        };
        assert_eq!(license.limits().users, Some(5));
        assert_eq!(license.limits().automations, Some(10));
        assert!(verify(
            &envelope(&claims, &key),
            &key.verifying_key(),
            uuid::Uuid::new_v4()
        )
        .is_err());
        assert!(verify(
            &envelope(&claims, &SigningKey::from_bytes(&[38; 32])),
            &key.verifying_key(),
            id
        )
        .is_err());
        let mut forged: serde_json::Value =
            serde_json::from_slice(&envelope(&claims, &key)).unwrap();
        let mut changed = claims.clone();
        changed["max_users"] = serde_json::json!(500);
        forged["payload"] =
            serde_json::json!(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).unwrap()));
        assert!(verify(
            &serde_json::to_vec(&forged).unwrap(),
            &key.verifying_key(),
            id
        )
        .is_err());
        for (field, value) in [
            ("product", serde_json::json!("another-app")),
            ("edition", serde_json::json!("admin")),
            ("version", serde_json::json!(2)),
            ("max_users", serde_json::json!(0)),
            ("max_automations", serde_json::json!(-1)),
            ("issued_at", serde_json::json!(now + 600)),
            ("customer", serde_json::json!("")),
            ("unexpected", serde_json::json!(true)),
        ] {
            let mut invalid = claims.clone();
            invalid[field] = value;
            assert!(
                verify(&envelope(&invalid, &key), &key.verifying_key(), id).is_err(),
                "{field}"
            );
        }
        assert!(verify(&vec![b'x'; 32769], &key.verifying_key(), id).is_err());
    }

    #[test]
    fn expiry_is_checked_on_each_operation_and_falls_back_to_individual_limits() {
        let id = uuid::Uuid::new_v4();
        let mut license = License::business_for_tests(id);
        assert!(license.limits().users.is_none());
        license.claims.as_mut().unwrap().expires_at = Some(chrono::Utc::now().timestamp() - 1);
        assert_eq!(license.limits().users, Some(1));
        assert_eq!(license.limits().automations, Some(3));
    }

    #[tokio::test]
    async fn installation_id_survives_restart_and_signed_expired_license_retains_its_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path().join("test.sqlite").to_str().unwrap())
            .await
            .unwrap();
        let license = License::load(&db).await.unwrap();
        assert_eq!(
            License::load(&db).await.unwrap().installation_id,
            license.installation_id
        );
        let mut expired = License::business_for_tests(license.installation_id);
        expired.claims.as_mut().unwrap().expires_at = Some(1);
        let key = SigningKey::from_bytes(&[37; 32]);
        expired.claims = Some(
            verify(
                &envelope(
                    &serde_json::to_value(expired.claims.as_ref().unwrap()).unwrap(),
                    &key,
                ),
                &key.verifying_key(),
                license.installation_id,
            )
            .unwrap(),
        );
        let status = expired.status(&db).await.unwrap();
        assert_eq!(status["license_status"], "expired");
        assert_eq!(status["edition"], "individual");
        assert_eq!(status["customer"], "Unit tests");
    }

    #[tokio::test]
    async fn business_allowances_permit_additional_automations_and_limited_licenses_obey_their_cap()
    {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path().join("test.sqlite").to_str().unwrap())
            .await
            .unwrap();
        let mut license = License::business_for_tests(uuid::Uuid::new_v4());
        license.claims.as_mut().unwrap().max_automations = Some(5);
        for n in 0..6 {
            let value =
                serde_json::json!({"id":format!("a{n}"),"created_at":"now","updated_at":"now"});
            assert_eq!(
                crate::db::create_automation(&db, &value, license.limits().automations)
                    .await
                    .unwrap(),
                n < 5
            );
        }
        license.claims.as_mut().unwrap().max_automations = None;
        assert!(crate::db::create_automation(
            &db,
            &serde_json::json!({"id":"unlimited","created_at":"now","updated_at":"now"}),
            license.limits().automations
        )
        .await
        .unwrap());
    }
}
