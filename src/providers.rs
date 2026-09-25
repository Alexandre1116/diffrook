use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct ModelLimits {
    pub max_input_chars: usize,
    pub max_output_tokens: u32,
}

pub fn limits_for(action: &str, requested_output: u64) -> ModelLimits {
    let (input, ceiling) = match action {
        "audit" => (180_000, 8_000),
        "solve_issue" => (120_000, 6_000),
        "fix_pr" => (120_000, 6_000),
        _ => (180_000, 6_000),
    };
    ModelLimits {
        max_input_chars: input,
        max_output_tokens: requested_output.clamp(256, ceiling as u64) as u32,
    }
}

pub async fn test_provider(provider: &Value, client: &reqwest::Client) -> Result<String> {
    let kind = string(provider, "kind")?;
    let model = string(provider, "default_model")?;
    let response = match kind {
        "openai" => {
            let base = trim_slash(string(provider, "base_url")?);
            let url = format!("{base}/models");
            let mut req = client.get(url);
            if let Some(key) = secret(provider, "api_key") {
                req = req.bearer_auth(key);
            }
            req.send()
                .await
                .context("requesting OpenAI compatible models")?
        }
        "ollama" => {
            let base = trim_slash(
                provider
                    .get("base_url")
                    .and_then(Value::as_str)
                    .unwrap_or("http://host.docker.internal:11434"),
            );
            let mut req = client.get(format!("{base}/api/tags"));
            if let Some(key) = secret(provider, "api_key") {
                req = req.bearer_auth(key);
            }
            req.send().await.context("requesting Ollama tags")?
        }
        "anthropic" => {
            let base = trim_slash(
                provider
                    .get("base_url")
                    .and_then(Value::as_str)
                    .unwrap_or("https://api.anthropic.com"),
            );
            let mut req = client
                .get(format!("{base}/v1/models?limit=1"))
                .header("anthropic-version", "2023-06-01");
            if let Some(key) = secret(provider, "api_key") {
                req = req.header("x-api-key", key);
            }
            req.send().await.context("requesting Anthropic models")?
        }
        other => bail!("unsupported provider kind: {other}"),
    };
    let status = response.status();
    if !status.is_success() {
        bail!("provider returned HTTP {status}");
    }
    Ok(format!("Connected to {kind}; configured model is {model}"))
}

pub async fn complete(
    provider: &Value,
    model: &str,
    system: &str,
    prompt: &str,
    max_output_tokens: u32,
    timeout: Duration,
    client: &reqwest::Client,
) -> Result<String> {
    if system.len().saturating_add(prompt.len()) > 180_000 {
        bail!("model request exceeds the 180000 character safety limit");
    }
    let kind = string(provider, "kind")?;
    let api_key = secret(provider, "api_key");
    let base = if let Some(base) = provider
        .get("base_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        base
    } else {
        match kind {
            "ollama" => "http://host.docker.internal:11434",
            "anthropic" => "https://api.anthropic.com",
            _ => bail!("OpenAI compatible provider requires base_url"),
        }
    };
    let base = trim_slash(base);
    let request = match kind {
        "openai" => {
            let mut req = client
                .post(format!("{base}/chat/completions"))
                .timeout(timeout)
                .header(reqwest::header::CONTENT_TYPE, "application/json");
            if let Some(key) = api_key {
                req = req.bearer_auth(key);
            }
            let mut body = json!({"model":model,"messages":[{"role":"system","content":system},{"role":"user","content":prompt}]});
            let token_parameter = provider["token_parameter"].as_str().unwrap_or("max_tokens");
            body[token_parameter] = json!(max_output_tokens);
            if provider["json_mode"] == true {
                body["response_format"] = json!({"type":"json_object"});
            }
            req.json(&body)
        }
        "ollama" => {
            let mut req = client.post(format!("{base}/api/chat")).timeout(timeout);
            if let Some(key) = api_key {
                req = req.bearer_auth(key);
            }
            req.json(&json!({"model":model,"stream":false,"format":"json","options":{"temperature":0,"num_predict":max_output_tokens},"messages":[{"role":"system","content":system},{"role":"user","content":prompt}]}))
        }
        "anthropic" => {
            let mut req = client
                .post(format!("{base}/v1/messages"))
                .timeout(timeout)
                .header("anthropic-version", "2023-06-01");
            if let Some(key) = api_key {
                req = req.header("x-api-key", key);
            }
            req.json(&json!({"model":model,"max_tokens":max_output_tokens,"system":system,"messages":[{"role":"user","content":prompt}]}))
        }
        other => bail!("unsupported provider kind: {other}"),
    };
    let response = request.send().await.context("sending model request")?;
    let status = response.status();
    let bytes = crate::network::read_body(response, 8 * 1024 * 1024).await?;
    let body: Value = serde_json::from_slice(&bytes).context("decoding model response")?;
    if !status.is_success() {
        bail!(
            "provider returned HTTP {status}: {}",
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("request rejected")
        );
    }
    let content = match kind {
        "openai" => body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        "ollama" => body.pointer("/message/content").and_then(Value::as_str),
        "anthropic" => body
            .get("content")
            .and_then(Value::as_array)
            .and_then(|v| {
                v.iter()
                    .find(|x| x.get("type").and_then(Value::as_str) == Some("text"))
            })
            .and_then(|x| x.get("text"))
            .and_then(Value::as_str),
        _ => None,
    }
    .ok_or_else(|| anyhow!("provider response did not contain text"))?;
    Ok(content.to_owned())
}

pub fn parse_json_object<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    let trimmed = text.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    };
    serde_json::from_str(body).context("model returned invalid JSON")
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("provider is missing {key}"))
}
fn secret<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}
fn trim_slash(s: &str) -> &str {
    s.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_limits_are_action_scoped_and_bounded() {
        assert_eq!(limits_for("audit", 99_000).max_output_tokens, 8_000);
        assert_eq!(limits_for("review", 1).max_output_tokens, 256);
        assert_eq!(limits_for("fix_pr", 3000).max_input_chars, 120_000);
    }
    #[test]
    fn strict_json_parser_accepts_fenced_json_but_rejects_prose() {
        let value: Value = parse_json_object("```json\n{\"findings\":[]}\n```").unwrap();
        assert!(value["findings"].is_array());
        assert!(parse_json_object::<Value>("Here you go: {\"ok\":true}").is_err());
    }
}
