use crate::network::SafeSend;
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
            req.send_safe()
                .await
                .context("requesting OpenAI compatible models")?
        }
        "ollama" => {
            let base = trim_slash(
                provider
                    .get("base_url")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("http://host.docker.internal:11434"),
            );
            let mut req = client.get(format!("{base}/api/tags"));
            if let Some(key) = secret(provider, "api_key") {
                req = req.bearer_auth(key);
            }
            req.send_safe().await.context("requesting Ollama tags")?
        }
        "anthropic" => {
            let base = trim_slash(
                provider
                    .get("base_url")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("https://api.anthropic.com"),
            );
            let mut req = client
                .get(format!("{base}/v1/models?limit=1"))
                .header("anthropic-version", "2023-06-01");
            if let Some(key) = secret(provider, "api_key") {
                req = req.header("x-api-key", key);
            }
            req.send_safe()
                .await
                .context("requesting Anthropic models")?
        }
        other => bail!("unsupported provider kind: {other}"),
    };
    let status = response.status();
    if !status.is_success() {
        bail!("provider returned HTTP {status}");
    }
    Ok(format!("Connected to {kind}; configured model is {model}"))
}

/// Tokens a provider reported for one model call. `reported` separates a
/// measured zero from a provider that never sent usage metadata, so the
/// overview can say "unavailable" instead of inventing a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub reported: bool,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input.unwrap_or(0) + self.output.unwrap_or(0)
    }
    pub fn add(&mut self, other: TokenUsage) {
        self.input = Some(
            self.input
                .unwrap_or(0)
                .saturating_add(other.input.unwrap_or(0)),
        );
        self.output = Some(
            self.output
                .unwrap_or(0)
                .saturating_add(other.output.unwrap_or(0)),
        );
        self.reported |= other.reported;
    }
    pub fn json(&self) -> Value {
        if self.reported {
            json!({"input_tokens": self.input, "output_tokens": self.output})
        } else {
            json!({"input_tokens": null, "output_tokens": null})
        }
    }
}

/// One model call. Grouped because the request, the HTTP client and the cell
/// that records what the call cost belong together.
pub struct Call<'a> {
    pub provider: &'a Value,
    pub model: &'a str,
    pub system: &'a str,
    pub prompt: &'a str,
    pub max_output_tokens: u32,
    pub timeout: Duration,
    pub client: &'a reqwest::Client,
    pub spent: &'a std::sync::Mutex<TokenUsage>,
}

/// Runs one model call and records what it cost as soon as the provider
/// answers, before any check on the response body. A gateway that bills for a
/// call and returns an empty or error body still gets its tokens counted.
pub async fn complete(request: Call<'_>) -> Result<String> {
    let Call {
        provider,
        model,
        system,
        prompt,
        max_output_tokens,
        timeout,
        client,
        spent,
    } = request;
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
    let response = request.send_safe().await.context("sending model request")?;
    let status = response.status();
    let bytes = crate::network::read_body(response, 8 * 1024 * 1024).await?;
    let body: Value = serde_json::from_slice(&bytes).context("decoding model response")?;
    // Count the call before checking the body: providers bill what they
    // generate, and a gateway that answers badly may already have spent it.
    let tokens = usage_for(kind, &body);
    if tokens.reported {
        spent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .add(tokens);
    }
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

/// Reads the token counters each provider returns. Ollama reports evaluation
/// counts, Anthropic and OpenAI compatible endpoints report usage objects, and
/// some OpenAI-compatible gateways use the Anthropic naming.
pub fn usage_for(kind: &str, body: &Value) -> TokenUsage {
    let number = |value: Option<&Value>| value.and_then(Value::as_u64);
    let read = if kind == "ollama" {
        TokenUsage {
            input: number(body.get("prompt_eval_count")),
            output: number(body.get("eval_count")),
            reported: false,
        }
    } else {
        let usage = body.get("usage");
        let pick = |primary: &str, secondary: &str| {
            number(usage.and_then(|u| u.get(primary).or_else(|| u.get(secondary))))
        };
        TokenUsage {
            input: pick("input_tokens", "prompt_tokens"),
            output: pick("output_tokens", "completion_tokens"),
            reported: false,
        }
    };
    TokenUsage {
        reported: read.input.is_some() || read.output.is_some(),
        ..read
    }
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
    #[test]
    fn token_usage_follows_each_provider_naming() {
        let openai = usage_for(
            "openai",
            &json!({"usage": {"prompt_tokens": 900, "completion_tokens": 120}}),
        );
        assert_eq!(openai.input, Some(900));
        assert_eq!(openai.output, Some(120));
        assert!(openai.reported);
        assert_eq!(openai.total(), 1020);
        let anthropic = usage_for(
            "anthropic",
            &json!({"usage": {"input_tokens": 40, "output_tokens": 8}}),
        );
        assert_eq!((anthropic.input, anthropic.output), (Some(40), Some(8)));
        let ollama = usage_for(
            "ollama",
            &json!({"prompt_eval_count": 700, "eval_count": 90}),
        );
        assert_eq!((ollama.input, ollama.output), (Some(700), Some(90)));
        let missing = usage_for("openai", &json!({}));
        assert!(!missing.reported, "no usage is not the same as no spend");
        assert_eq!(missing.total(), 0);
        let partial = usage_for("openai", &json!({"usage": {"completion_tokens": 7}}));
        assert_eq!((partial.input, partial.output), (None, Some(7)));
        let mut running = TokenUsage::default();
        running.add(openai);
        running.add(missing);
        assert_eq!((running.input, running.output), (Some(900), Some(120)));
        assert!(running.reported);
        assert_eq!(
            TokenUsage::default().json(),
            json!({"input_tokens": null, "output_tokens": null})
        );
    }

    /// A gateway that bills the call and answers with no text: the spend is real
    /// and must reach the run, even though the call itself failed.
    #[tokio::test]
    async fn a_provider_response_without_text_still_records_its_spend() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Read the request before answering, then keep the socket open long
            // enough for the client to take the response.
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let body = r#"{"usage":{"prompt_tokens":120,"completion_tokens":30}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        let spent = std::sync::Mutex::new(TokenUsage::default());
        let provider = json!({"kind":"openai","base_url":format!("http://{addr}"),"api_key":"key"});
        let outcome = crate::network::with_private_mock(async {
            complete(Call {
                provider: &provider,
                model: "gpt-5.2",
                system: "s",
                prompt: "p",
                max_output_tokens: 16,
                timeout: Duration::from_secs(5),
                client: &reqwest::Client::new(),
                spent: &spent,
            })
            .await
        })
        .await;
        assert!(
            outcome.is_err(),
            "a response with no text is still a failure"
        );
        let usage = *spent.lock().unwrap();
        assert!(usage.reported, "the provider reported what it spent");
        assert_eq!((usage.input, usage.output), (Some(120), Some(30)));
        assert_eq!(usage.total(), 150);
    }
}
