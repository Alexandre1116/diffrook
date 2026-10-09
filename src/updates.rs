use crate::core::{error, AppState};
use anyhow::{bail, Context, Result};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use flate2::read::GzDecoder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

const RELEASES: &str = "https://api.github.com/repos/Alexandre1116/diffrook/releases?per_page=25";
const MAX_ARCHIVE: usize = 180 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 300 * 1024 * 1024;

pub fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(900));
        loop {
            if let Err(e) = refresh(&state).await {
                tracing::warn!("release check failed: {e:#}");
                let mut value = state.updates.write().await;
                value["error"] = json!("GitHub release check failed");
            }
            if policy(&state).await.as_deref() == Some("automatic") {
                if let Some(tag) = latest_available(&state).await {
                    if let Err(e) = install(&state, &tag).await {
                        tracing::error!("automatic update {tag} failed: {e:#}");
                    }
                }
            }
            interval.tick().await;
        }
    });
}

async fn refresh(state: &AppState) -> Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .get(RELEASES)
        .header(
            reqwest::header::USER_AGENT,
            concat!("Diffrook/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .context("requesting GitHub releases")?;
    let status = response.status();
    let bytes = crate::network::read_body(response, 2 * 1024 * 1024).await?;
    if !status.is_success() {
        bail!("GitHub releases API returned HTTP {status}");
    }
    let raw: Vec<Value> = serde_json::from_slice(&bytes).context("decoding GitHub release list")?;
    let current =
        semver::Version::parse(env!("CARGO_PKG_VERSION")).context("parsing current version")?;
    let mut releases = Vec::new();
    for release in raw {
        if release["draft"] == true {
            continue;
        }
        let tag = release["tag_name"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches('v');
        let Ok(version) = semver::Version::parse(tag) else {
            continue;
        };
        let newer = version > current;
        let asset_name = format!("diffrook-linux-{}.tar.gz", std::env::consts::ARCH);
        let asset = release["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["name"] == asset_name));
        releases.push(json!({
            "tag": release["tag_name"], "version": version.to_string(), "name": release["name"],
            "prerelease": release["prerelease"] == true, "published_at": release["published_at"],
            "url": release["html_url"], "newer": newer,
            "supported": cfg!(target_os = "linux") && asset.is_some(), "asset_url": asset.and_then(|a| a["browser_download_url"].as_str()),
            "asset_digest": asset.and_then(|a| a["digest"].as_str()),
            "asset_size": asset.and_then(|a| a["size"].as_u64()).unwrap_or(0)
        }));
    }
    releases.sort_by(|a, b| {
        semver::Version::parse(b["version"].as_str().unwrap_or("0.0.0"))
            .ok()
            .cmp(&semver::Version::parse(a["version"].as_str().unwrap_or("0.0.0")).ok())
    });
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.checked_at',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(&now).execute(&state.db).await?;
    *state.updates.write().await = json!({"current_version":current.to_string(),"releases":releases,"checked_at":now,"error":null});
    Ok(())
}

async fn policy(state: &AppState) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM app_settings WHERE key='updates.policy'")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
}

async fn latest_available(state: &AppState) -> Option<String> {
    let failed = tokio::fs::read_to_string(state.data_dir.join("failed-update.txt"))
        .await
        .ok();
    state.updates.read().await["releases"]
        .as_array()?
        .iter()
        .find(|r| {
            r["newer"] == true && r["supported"] == true && failed.as_deref() != r["tag"].as_str()
        })
        .and_then(|r| r["tag"].as_str().map(str::to_owned))
}

async fn install(state: &AppState, tag: &str) -> Result<()> {
    let _update_guard = state.update_lock.lock().await;
    anyhow::ensure!(
        can_stage_update(&state.data_dir)?,
        "an update is already staged for restart"
    );
    let active_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status='running'")
        .fetch_one(&state.db)
        .await?;
    anyhow::ensure!(
        active_runs == 0,
        "wait for running automations to finish before installing an update"
    );
    let release = state.updates.read().await["releases"]
        .as_array()
        .and_then(|rs| {
            rs.iter()
                .find(|r| r["tag"] == tag && r["newer"] == true && r["supported"] == true)
        })
        .cloned()
        .context("release has no compatible update package")?;
    let tag_safe: String = tag
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect();
    anyhow::ensure!(
        !tag_safe.is_empty() && tag_safe == tag,
        "invalid release tag"
    );
    let url = release["asset_url"]
        .as_str()
        .context("release package URL is missing")?;
    let parsed_url = url::Url::parse(url)?;
    anyhow::ensure!(
        parsed_url.scheme() == "https" && parsed_url.host_str() == Some("github.com"),
        "release package must be hosted on GitHub over HTTPS"
    );
    let size = release["asset_size"].as_u64().unwrap_or(0);
    anyhow::ensure!(
        size > 0 && size <= MAX_ARCHIVE as u64,
        "release package size is invalid"
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(180))
        .build()?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "Diffrook updater")
        .send()
        .await
        .context("downloading release package")?;
    let status = response.status();
    let archive = crate::network::read_body(response, MAX_ARCHIVE).await?;
    if !status.is_success() {
        bail!("release download returned HTTP {status}");
    }
    let digest = release["asset_digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .context("GitHub release metadata does not provide a SHA-256 checksum")?;
    let actual = hex::encode(Sha256::digest(&archive));
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(digest),
        "release package checksum does not match GitHub metadata"
    );
    let data_dir = state.data_dir.clone();
    let dir = tokio::task::spawn_blocking(move || extract_release(&data_dir, &tag_safe, &archive))
        .await??;
    let _ = tokio::fs::remove_file(state.data_dir.join("failed-update.txt")).await;
    let descriptor = json!({"version":release["version"],"tag":tag,"path":dir.to_string_lossy(),"attempts":0,"confirmed":false});
    let tmp = state.data_dir.join("active-update.json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(&descriptor)?).await?;
    tokio::fs::rename(tmp, state.data_dir.join("active-update.json")).await?;
    tracing::warn!(
        "Installing Diffrook {}; the service will restart",
        release["version"]
    );
    // The HTTP server drains this response before exiting. Signal while still
    // holding the worker's claim lock, so no queued job starts during restart.
    state.restart.send_replace(true);
    Ok(())
}

fn can_stage_update(data_dir: &Path) -> Result<bool> {
    let path = data_dir.join("active-update.json");
    if !path.exists() {
        return Ok(true);
    }
    let descriptor: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    // Keep the confirmed descriptor so container restarts still launch the
    // installed binary, while allowing its next update to replace it.
    Ok(descriptor["confirmed"] == true
        && descriptor["version"].as_str() == Some(env!("CARGO_PKG_VERSION")))
}

fn extract_release(data_dir: &Path, tag: &str, bytes: &[u8]) -> Result<PathBuf> {
    let parent = data_dir.join("updates");
    std::fs::create_dir_all(&parent)?;
    let stage = parent.join(format!("{tag}.tmp"));
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    std::fs::create_dir(&stage)?;
    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut total = 0u64;
    let mut binary = false;
    let mut web_index = false;
    for (total_entries, entry) in archive.entries()?.enumerate() {
        let mut entry = entry?;
        anyhow::ensure!(
            total_entries < 20_000,
            "release package contains too many files"
        );
        let path = entry.path()?.into_owned();
        anyhow::ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_))),
            "release archive has an unsafe path"
        );
        anyhow::ensure!(
            entry.header().entry_type().is_file() || entry.header().entry_type().is_dir(),
            "release archive may contain regular files and directories only"
        );
        let parts: Vec<_> = path
            .components()
            .filter_map(|c| {
                if let Component::Normal(s) = c {
                    Some(s.to_string_lossy())
                } else {
                    None
                }
            })
            .collect();
        anyhow::ensure!(
            parts.first().is_some_and(|p| p == "diffrook" || p == "web"),
            "unexpected file in release archive"
        );
        if parts[0] == "diffrook" {
            anyhow::ensure!(parts.len() == 1, "unexpected binary archive path");
            binary = true;
        }
        if parts.first().is_some_and(|p| p == "web")
            && parts.get(1).is_some_and(|p| p == "index.html")
        {
            web_index = true;
        }
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(stage.join(&path))?;
            continue;
        }
        total = total.saturating_add(entry.size());
        anyhow::ensure!(
            total <= MAX_EXTRACTED,
            "release expands beyond the size limit"
        );
        let out = stage.join(&path);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(out)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    anyhow::ensure!(
        binary && web_index,
        "release package is missing the application or web interface"
    );
    let binary_path = stage.join("diffrook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o700))?;
    }
    let final_path = parent.join(tag);
    if final_path.exists() {
        std::fs::remove_dir_all(&final_path)?;
    }
    std::fs::rename(stage, &final_path)?;
    Ok(final_path)
}

pub async fn api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, false).await {
        return r;
    }
    let mut value = state.updates.read().await.clone();
    value["policy"] = json!(policy(&state).await.unwrap_or_else(|| "manual".into()));
    Json(value).into_response()
}

pub async fn set_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, true).await {
        return r;
    }
    let Some(mode @ ("manual" | "automatic")) = body["mode"].as_str() else {
        return error(
            StatusCode::BAD_REQUEST,
            "Choose manual or automatic updates",
        );
    };
    if mode == "automatic" && !cfg!(target_os = "linux") {
        return error(
            StatusCode::BAD_REQUEST,
            "Automatic updates require a Linux Docker container",
        );
    }
    if mode == "automatic" && !["x86_64", "aarch64"].contains(&std::env::consts::ARCH) {
        return error(
            StatusCode::BAD_REQUEST,
            "Automatic updates are available for x86-64 and ARM64 Linux containers",
        );
    }
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.policy',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(mode).execute(&state.db).await { return error(StatusCode::INTERNAL_SERVER_ERROR, e); }
    if mode == "automatic" {
        if let Some(tag) = latest_available(&state).await {
            if let Err(e) = install(&state, &tag).await {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("Automatic update failed: {e:#}"),
                );
            }
        }
    }
    Json(json!({"mode":mode})).into_response()
}

pub async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, true).await {
        return r;
    }
    let Some(tag) = body["tag"].as_str() else {
        return error(StatusCode::BAD_REQUEST, "Release tag is required");
    };
    if !cfg!(target_os = "linux") {
        return error(
            StatusCode::BAD_REQUEST,
            "In-container updates require Linux",
        );
    }
    match install(&state, tag).await {
        Ok(()) => Json(json!({"ok":true,"restarting":true})).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("Update failed: {e:#}")),
    }
}

pub fn exec_active(data_dir: &Path) -> Result<bool> {
    let path = data_dir.join("active-update.json");
    if !path.exists() {
        return Ok(false);
    }
    let descriptor: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let version = descriptor["version"]
        .as_str()
        .context("invalid active update version")?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let target = semver::Version::parse(version)?;
    let root = PathBuf::from(
        descriptor["path"]
            .as_str()
            .context("invalid active update path")?,
    );
    anyhow::ensure!(
        root.starts_with(data_dir.join("updates")),
        "active update points outside its data directory"
    );
    if target < current {
        std::fs::remove_file(path)?;
        return Ok(false);
    }
    if target == current {
        if std::env::current_exe()
            .ok()
            .is_some_and(|exe| !exe.starts_with(root.join("diffrook")))
        {
            std::fs::remove_file(path)?;
        }
        return Ok(false);
    }
    if descriptor["attempts"].as_u64().unwrap_or(0) >= 1 && descriptor["confirmed"] != true {
        let tag = descriptor["tag"].as_str().unwrap_or(version);
        std::fs::write(data_dir.join("failed-update.txt"), tag)?;
        std::fs::remove_file(path)?;
        tracing::error!("Diffrook update {tag} did not start; restored the container version");
        return Ok(false);
    }
    let mut descriptor = descriptor;
    descriptor["attempts"] = json!(1);
    let tmp = data_dir.join("active-update.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&descriptor)?)?;
    std::fs::rename(tmp, path)?;
    let bin = root.join("diffrook");
    anyhow::ensure!(bin.is_file(), "active update binary is missing");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(bin)
            .args(std::env::args_os().skip(1))
            .env("DIFFROOK_WEB_DIR", root.join("web"))
            .exec();
        Err(anyhow::anyhow!("restarting Diffrook from update: {err}"))
    }
    #[cfg(not(unix))]
    {
        Ok(false)
    }
}

pub fn confirm_active(data_dir: &Path) -> Result<()> {
    let path = data_dir.join("active-update.json");
    if !path.exists() {
        return Ok(());
    }
    let mut descriptor: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    if descriptor["version"].as_str() == Some(env!("CARGO_PKG_VERSION"))
        && descriptor["attempts"].as_u64().unwrap_or(0) == 1
    {
        descriptor["confirmed"] = json!(true);
        let tmp = data_dir.join("active-update.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&descriptor)?)?;
        std::fs::rename(tmp, path)?;
    }
    Ok(())
}

pub async fn restart_signal(state: &AppState) {
    let mut rx = state.restart.subscribe();
    let _ = rx.changed().await;
}

#[cfg(test)]
mod rollback_tests {
    use super::*;

    #[test]
    fn unconfirmed_failed_start_is_rolled_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let updates = dir.path().join("updates/v999.0.0");
        std::fs::create_dir_all(&updates).unwrap();
        let descriptor = json!({"version":"999.0.0","tag":"v999.0.0","path":updates,"attempts":1,"confirmed":false});
        std::fs::write(
            dir.path().join("active-update.json"),
            serde_json::to_vec(&descriptor).unwrap(),
        )
        .unwrap();
        assert!(!exec_active(dir.path()).unwrap());
        assert!(!dir.path().join("active-update.json").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("failed-update.txt")).unwrap(),
            "v999.0.0"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_current_update_can_be_replaced_but_pending_update_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("active-update.json");
        assert!(can_stage_update(dir.path()).unwrap());
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"version":env!("CARGO_PKG_VERSION"),"confirmed":true}))
                .unwrap(),
        )
        .unwrap();
        assert!(can_stage_update(dir.path()).unwrap());
        assert!(path.exists(), "restart descriptor must be retained");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"version":env!("CARGO_PKG_VERSION"),"confirmed":false}))
                .unwrap(),
        )
        .unwrap();
        assert!(!can_stage_update(dir.path()).unwrap());
        std::fs::write(&path, "invalid").unwrap();
        assert!(can_stage_update(dir.path()).is_err());
    }
    use std::io::Write;
    #[test]
    fn extracts_only_bounded_regular_files_from_release_package() {
        let dir = tempfile::tempdir().unwrap();
        let raw = tempfile::NamedTempFile::new().unwrap();
        let encoder =
            flate2::write::GzEncoder::new(raw.reopen().unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, bytes) in [
            ("diffrook", b"binary".as_slice()),
            ("web/index.html", b"index".as_slice()),
            ("web/assets/app.js", b"app".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, bytes).unwrap();
        }
        let mut encoder = builder.into_inner().unwrap();
        encoder.flush().unwrap();
        encoder.finish().unwrap();
        let bytes = std::fs::read(raw.path()).unwrap();
        let extracted = extract_release(dir.path(), "v1.2.3", &bytes).unwrap();
        assert_eq!(
            std::fs::read(extracted.join("web/assets/app.js")).unwrap(),
            b"app"
        );
        assert!(extract_release(dir.path(), "bad", b"not gzip").is_err());
    }

    #[test]
    fn rejects_archive_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut builder = tar::Builder::new(&mut compressed);
            let mut header = tar::Header::new_gnu();
            let malicious_path = b"web/../../escaped";
            header.as_mut_bytes()[..malicious_path.len()].copy_from_slice(malicious_path);
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, b"oops".as_slice()).unwrap();
            builder.finish().unwrap();
        }
        let bytes = compressed.finish().unwrap();
        assert!(extract_release(dir.path(), "v1.2.4", &bytes).is_err());
        assert!(!dir.path().parent().unwrap().join("escaped").exists());
    }
}
