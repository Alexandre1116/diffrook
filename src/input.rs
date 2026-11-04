//! Strict input schemas. Response metadata is accepted for editor round trips,
//! but never persisted from the request.
use serde::{Deserialize, Serialize};
use serde_json::Value;

macro_rules! input {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Deserialize, Serialize)]
        #[serde(deny_unknown_fields)]
        struct $name {
            $(#[serde(skip_serializing_if = "Option::is_none")]
            $field: Option<$ty>,)*
        }
    };
}
macro_rules! object {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        input!($name {
            $($field: $ty,)*
            id: String, created_at: String, updated_at: String,
            has_token: bool, has_webhook_secret: bool, has_api_key: bool, has_url: bool
        });
    };
}
object!(Connection {
    name: String,
    kind: String,
    base_url: String,
    token: String,
    webhook_secret: String,
    bot_username: String
});
object!(Provider {
    name: String,
    kind: String,
    base_url: String,
    api_key: String,
    default_model: String,
    token_parameter: String,
    json_mode: bool
});
object!(NotificationProvider {
    name: String,
    kind: String,
    url: String
});
object!(Automation { name: String, description: String, enabled: bool,
    connection_id: String, provider_id: String, model: String,
    repositories: Vec<String>, action: String, instructions: String,
    trigger: Trigger, filters: Filters, limits: Limits, fix: Fix,
    notifications: Vec<Notification> });
input!(Trigger { events: Vec<String>, command: String, schedule_enabled: bool,
    cron: String, timezone: String, schedule_target: String, branch: String });
input!(Filters { labels: Vec<String>, ignore_drafts: bool, allowed_actors: Vec<String>,
    ignore_paths: Vec<String> });
input!(Limits {
    max_files: u64,
    max_file_bytes: u64,
    max_context_chars: u64,
    max_output_tokens: u64,
    timeout_seconds: u64,
    max_fix_files: u64
});
input!(Fix {
    mode: String,
    branch_prefix: String
});
input!(Notification {
    id: String,
    kind: String,
    provider_id: String,
    url: String,
    has_url: bool
});

pub fn sanitize(kind: &str, value: Value) -> Result<Value, String> {
    if kind == "automations" {
        for field in ["trigger", "filters", "limits", "fix"] {
            if value.get(field).is_some_and(|v| !v.is_object()) {
                return Err(format!("{field} must be an object"));
            }
        }
        if let Some(items) = value.get("notifications").and_then(Value::as_array) {
            if items.iter().any(|v| !v.is_object()) {
                return Err("Notification destinations must be objects".into());
            }
        }
    }
    fn parse<T: serde::de::DeserializeOwned + Serialize>(v: Value) -> Result<Value, String> {
        let input: T = serde_json::from_value(v).map_err(|e| e.to_string())?;
        serde_json::to_value(input).map_err(|e| e.to_string())
    }
    let mut v = match kind {
        "connections" => parse::<Connection>(value),
        "providers" => parse::<Provider>(value),
        "notification_providers" => parse::<NotificationProvider>(value),
        "automations" => parse::<Automation>(value),
        _ => return Err("Unknown object kind".into()),
    }?;
    for key in [
        "id",
        "created_at",
        "updated_at",
        "has_token",
        "has_webhook_secret",
        "has_api_key",
        "has_url",
    ] {
        v.as_object_mut().unwrap().remove(key);
    }
    if let Some(ns) = v.get_mut("notifications").and_then(Value::as_array_mut) {
        for n in ns {
            n.as_object_mut().unwrap().remove("has_url");
        }
    }
    Ok(v)
}
