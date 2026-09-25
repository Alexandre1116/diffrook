use anyhow::{bail, Context, Result};

/// Bound remote bodies even when Content-Length is absent or inaccurate.
pub async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        bail!("remote response exceeds the {} byte limit", limit);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.context("reading remote response")? {
        if body.len().saturating_add(chunk.len()) > limit {
            bail!("remote response exceeds the {} byte limit", limit);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
