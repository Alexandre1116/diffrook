use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub async fn send(notification: &Value, message: &str, client: &reqwest::Client) -> Result<()> {
    let kind = notification
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    let url = notification
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("notification is missing url"))?;
    let response = match kind {
        "discord" => client.post(url).json(&json!({"content":bounded(message,1900),"allowed_mentions":{"parse":[]}})).send().await,
        "slack" => client.post(url).json(&json!({"text":bounded(message,35000)})).send().await,
        "teams" => client.post(url).json(&json!({"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive","contentUrl":null,"content":{"$schema":"http://adaptivecards.io/schemas/adaptive-card.json","type":"AdaptiveCard","version":"1.2","body":[{"type":"TextBlock","text":"Diffrook review","weight":"Bolder"},{"type":"TextBlock","text":bounded(message,20000),"wrap":true}]}}]})).send().await,
        "webhook" => client.post(url).json(&json!({"text":bounded(message,28000)})).send().await,
        _ => bail!("unsupported external notification kind: {kind}"),
    }.context("sending external notification")?;
    if !response.status().is_success() {
        bail!("notification endpoint returned HTTP {}", response.status());
    }
    Ok(())
}

fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1
    }
    format!("{}\n\nOutput was truncated.", &text[..end])
}

pub fn render_findings(summary: &str, findings: &[crate::types::Finding]) -> String {
    let mut out = format!("## Diffrook\n\n{summary}\n");
    if findings.is_empty() {
        out.push_str("\nNo findings.\n");
    }
    for f in findings {
        out.push_str(&format!("\n- **{}: {}**", f.severity, f.title));
        if !f.file.is_empty() {
            if let Some(line) = f.line {
                out.push_str(&format!(" (`{}:{line}`)", f.file));
            } else {
                out.push_str(&format!(" (`{}`)", f.file));
            }
        }
        out.push_str(&format!("\n  {}", f.explanation));
        if !f.suggestion.is_empty() {
            out.push_str(&format!("\n  Suggestion: {}", f.suggestion));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn findings_render_with_paths_and_summary() {
        let text = render_findings(
            "Review complete",
            &[crate::types::Finding {
                severity: "high".into(),
                title: "Null panic".into(),
                file: "src/a.rs".into(),
                line: Some(9),
                explanation: "Value may be absent.".into(),
                suggestion: "Check it first.".into(),
            }],
        );
        assert!(text.contains("src/a.rs:9"));
        assert!(text.contains("Review complete"));
    }
}
