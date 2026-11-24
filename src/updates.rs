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

const RELEASES: &str = "https://api.github.com/repos/Alexandre1116/diffrook/releases?per_page=100";
const MAX_ARCHIVE: usize = 180 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 300 * 1024 * 1024;
/// How often a healthy instance asks GitHub for releases.
const CHECK_INTERVAL: Duration = Duration::from_secs(900);
/// How soon it tries again after a check that GitHub or the network rejected.
const FIRST_RETRY: Duration = Duration::from_secs(60);
/// The longest gap between attempts once failures keep repeating.
const MAX_RETRY: Duration = Duration::from_secs(3600);
/// How long a manual "check now" must wait before it may hit GitHub again.
const MANUAL_CHECK_COOLDOWN: i64 = 15;
/// The longest wait this instance will sleep because GitHub asked for it. A
/// longer hint is treated as a broken response rather than an hour of silence.
const MAX_HINT: Duration = Duration::from_secs(6 * 3600);
/// A release that failed to install this often is left alone until an
/// administrator asks again, so a broken package is not downloaded forever.
const INSTALL_FAILURE_LIMIT: u32 = 3;

/// How long to wait before the next release check. A wait GitHub asked for
/// wins, because it is the only value that matches the quota this instance
/// really has; plain failures back off to `MAX_RETRY`.
fn next_wait(failures: u32, hinted: Option<Duration>) -> Duration {
    match hinted {
        Some(wait) => wait.clamp(FIRST_RETRY, MAX_HINT),
        None => {
            let step = 1u32 << failures.saturating_sub(1).min(6);
            FIRST_RETRY.saturating_mul(step).min(MAX_RETRY)
        }
    }
}

/// What the release loop should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomaticOutcome {
    /// Nothing new to install.
    Idle,
    /// A newer release is on this instance's channel but the update must wait.
    Deferred,
    /// The update was staged and the service is restarting.
    Restarting,
}

pub fn start(state: AppState) {
    // A queued install is the administrator's own request. It runs on its own
    // clock, without GitHub, so an outage cannot strand the instance on a
    // release the page already promised to replace.
    let queued = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            if pending_install(&queued).await.is_none() {
                continue;
            }
            match install_due_update(&queued).await {
                Ok(AutomaticOutcome::Restarting) => return,
                Ok(_) => {}
                Err(e) => tracing::error!("queued update failed: {e:#}"),
            }
        }
    });
    tokio::spawn(async move {
        // A failure recorded before the restart stays visible after it.
        let stored = install_error(&state).await;
        if !stored.is_null() {
            state.updates.write().await["install_error"] = stored;
        }
        let mut failures = 0u32;
        loop {
            if let Err(e) = refresh(&state).await {
                failures += 1;
                tracing::warn!("release check failed: {e:#}");
                let hinted = state.updates.read().await["retry_after"]
                    .as_u64()
                    .map(Duration::from_secs);
                tokio::time::sleep(next_wait(failures, hinted)).await;
                continue;
            }
            failures = 0;
            // An update can be held back by running automations. Retry the
            // install on a short timer without asking GitHub again: the release
            // list cannot have changed, and every fetch spends a quota.
            let mut waited = Duration::ZERO;
            loop {
                match install_due_update(&state).await {
                    Ok(AutomaticOutcome::Restarting) => return,
                    Ok(AutomaticOutcome::Deferred) => {
                        let step = Duration::from_secs(120);
                        if waited + step >= CHECK_INTERVAL {
                            break;
                        }
                        tokio::time::sleep(step).await;
                        waited += step;
                    }
                    Ok(AutomaticOutcome::Idle) => break,
                    Err(e) => {
                        tracing::error!("automatic update failed: {e:#}");
                        break;
                    }
                }
            }
            tokio::time::sleep(CHECK_INTERVAL.saturating_sub(waited)).await;
        }
    });
}

/// Why a release check failed, and how long GitHub asked the instance to wait.
struct CheckFailure {
    message: String,
    retry_after: Option<u64>,
}

impl CheckFailure {
    fn plain<E: Into<anyhow::Error>>(e: E) -> Self {
        Self {
            message: format!("{:#}", e.into()),
            retry_after: None,
        }
    }

    fn limited<E: Into<anyhow::Error>>(
        e: E,
        status: StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> Self {
        Self {
            message: format!("{:#}", e.into()),
            retry_after: retry_after(status, headers).map(|wait| wait.as_secs()),
        }
    }
}

/// Replaces the cached release list. Every failure is written into
/// `state.updates`, because that is the value Settings reads to decide what the
/// administrator may believe about being up to date.
async fn refresh(state: &AppState) -> Result<()> {
    // Every attempt is stamped, successful or not, so a manual "check now"
    // cannot be used to hammer GitHub while checks are already failing.
    let attempted = chrono::Utc::now().to_rfc3339();
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.last_attempt',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(&attempted)
        .execute(&state.db)
        .await
    {
        tracing::warn!("could not record the release check attempt: {e:#}");
    }
    match load_releases(state).await {
        Ok(mut value) => {
            // A healthy check must not erase why the last automatic install
            // failed, as long as that release is still being offered.
            let previous = state.updates.read().await.clone();
            let stale = previous["install_error"]["tag"]
                .as_str()
                .is_some_and(|tag| {
                    value["releases"]
                        .as_array()
                        .is_some_and(|rs| rs.iter().any(|r| r["tag"] == tag))
                });
            if stale {
                value["install_error"] = previous["install_error"].clone();
            }
            *state.updates.write().await = value;
            Ok(())
        }
        Err(failure) => {
            let mut value = state.updates.write().await;
            value["error"] = json!(failure.message);
            value["retry_after"] = match failure.retry_after {
                Some(seconds) => json!(seconds),
                None => Value::Null,
            };
            Err(anyhow::anyhow!(failure.message))
        }
    }
}

async fn load_releases(state: &AppState) -> std::result::Result<Value, CheckFailure> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(CheckFailure::plain)?;
    let response = client
        .get(RELEASES)
        .header(
            reqwest::header::USER_AGENT,
            concat!("Diffrook/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .context("requesting GitHub releases")
        .map_err(CheckFailure::plain)?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = crate::network::read_body(response, 4 * 1024 * 1024)
        .await
        .map_err(CheckFailure::plain)?;
    if !status.is_success() {
        return Err(CheckFailure::limited(
            anyhow::anyhow!("GitHub releases API returned HTTP {status}"),
            status,
            &headers,
        ));
    }
    let raw: Vec<Value> = serde_json::from_slice(&bytes)
        .context("decoding GitHub release list")
        .map_err(CheckFailure::plain)?;
    let current = match semver::Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) => version,
        Err(e) => return Err(CheckFailure::plain(anyhow::anyhow!(e))),
    };
    let failed = tokio::fs::read_to_string(state.data_dir.join("failed-update.txt"))
        .await
        .unwrap_or_default();
    let failed = failed.trim();
    let install_failures = install_failures(state).await;
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
        // One rule decides whether this release can be installed here, and the
        // installer enforces the same one, so the page never offers a button
        // that only fails after a 180 MB download.
        let problem = if cfg!(target_os = "linux") {
            package_problem(asset)
        } else {
            Some("in-container updates need a Linux container")
        };
        releases.push(json!({
            "tag": release["tag_name"], "version": version.to_string(), "name": release["name"],
            "prerelease": release["prerelease"] == true, "published_at": release["published_at"],
            "url": release["html_url"], "newer": newer,
            "rolled_back": !failed.is_empty() && release["tag_name"].as_str() == Some(failed),
            "install_failures": release_failures(&install_failures, release["tag_name"].as_str().unwrap_or("")),
            "supported": problem.is_none(), "unsupported_reason": problem,
            "asset_url": asset.and_then(|a| a["browser_download_url"].as_str()),
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
    // A settings write must not throw away a release list that arrived safely.
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.checked_at',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(&now).execute(&state.db).await {
        tracing::warn!("could not record the release check time: {e:#}");
    }
    Ok(
        json!({"current_version":current.to_string(),"releases":releases,"checked_at":now,"error":null,"retry_after":null,"last_attempt":now}),
    )
}

/// The wait GitHub asked for, if any. `Retry-After` always counts. The rate
/// limit clock only counts when GitHub also says the quota is used up, so a
/// transient 502 cannot silence release checks for an hour.
fn retry_after(status: StatusCode, headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    if let Some(seconds) = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(seconds));
    }
    let refused = matches!(status.as_u16(), 403 | 429);
    let quota_spent = headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u32>().ok())
        == Some(0);
    if !refused || !quota_spent {
        return None;
    }
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())?;
    let seconds = reset - chrono::Utc::now().timestamp();
    // Add a small margin so the next attempt never lands right on the boundary.
    Some(Duration::from_secs((seconds + 5).max(0) as u64))
}

/// Whether the updater can fetch and verify a package at all: an HTTPS asset on
/// GitHub, a SHA-256 digest to check it against, and a size within the limit.
/// `stage_release` insists on the same, so the page never offers an Install
/// button for a package the installer would refuse.
fn package_installable(url: Option<&str>, digest: Option<&str>, size: u64) -> bool {
    let address = url.and_then(|url| url::Url::parse(url).ok());
    let checksum_ok = digest
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()));
    address.is_some_and(|address| {
        address.scheme() == "https" && address.host_str() == Some("github.com")
    }) && checksum_ok
        && size > 0
        && size <= MAX_ARCHIVE as u64
}

/// Why a release cannot be installed here, in one line, or `None` when it can.
fn package_problem(asset: Option<&Value>) -> Option<&'static str> {
    let Some(asset) = asset else {
        return Some("no update package is published for this platform");
    };
    if asset["digest"].as_str().is_none() {
        return Some("GitHub publishes no checksum for this package");
    }
    if !package_installable(
        asset["browser_download_url"].as_str(),
        asset["digest"].as_str(),
        asset["size"].as_u64().unwrap_or(0),
    ) {
        return Some("the update package for this platform cannot be installed here");
    }
    None
}

/// One release's failed automatic installs. Counts expire: three failures in a
/// row within six hours stop the updater, while a day of network trouble must
/// not park an instance on an old version forever.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct FailureCount {
    count: u32,
    last_at: String,
}

const FAILURE_WINDOW: i64 = 6 * 3600;

impl FailureCount {
    /// Whether the last counted failure still counts as recent.
    fn is_fresh(&self) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.last_at)
            .map(|when| {
                chrono::Utc::now()
                    .signed_duration_since(when.with_timezone(&chrono::Utc))
                    .num_seconds()
                    < FAILURE_WINDOW
            })
            .unwrap_or(false)
    }

    fn parked(&self) -> bool {
        self.count >= INSTALL_FAILURE_LIMIT && self.is_fresh()
    }
}

/// Releases whose automatic install failed, counted in the database so a broken
/// package is not downloaded again after every restart.
async fn install_failures(state: &AppState) -> std::collections::HashMap<String, FailureCount> {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_settings WHERE key='updates.install_failures'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|stored| serde_json::from_str(&stored).ok())
    .unwrap_or_default()
}

fn release_failures(
    failures: &std::collections::HashMap<String, FailureCount>,
    tag: &str,
) -> Value {
    match failures.get(tag) {
        Some(record) => json!(record.count),
        None => Value::Null,
    }
}

/// The last automatic install that failed, kept in the database so the reason
/// outlives the restart that follows it.
async fn install_error(state: &AppState) -> Value {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_settings WHERE key='updates.install_error'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|stored| serde_json::from_str(&stored).ok())
    .unwrap_or(Value::Null)
}

/// Marks a failure that will repeat: the package itself is wrong, so trying
/// again only spends bandwidth. Anything else (network, quota, a run that
/// started under the lock) is the moment's fault, not the release's.
#[derive(Debug)]
struct Fatal;

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the release package cannot be installed here")
    }
}

impl std::error::Error for Fatal {}

fn fatal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Fatal>().is_some()
}

/// The install nobody asked for any more: the queued request that authorized it
/// was cancelled, or replaced by a request for a different release, while the
/// package was being fetched. Nothing is wrong with the release.
#[derive(Debug)]
struct Withdrawn;

impl std::fmt::Display for Withdrawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("that update is no longer queued")
    }
}

impl std::error::Error for Withdrawn {}

fn withdrawn(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Withdrawn>().is_some()
}

/// Counts a failed install and leaves the reason where Settings can show it.
/// Only a broken package counts towards leaving a release alone.
async fn record_install_failure(state: &AppState, tag: &str, message: &str, counted: bool) {
    let mut counts = install_failures(state).await;
    let attempts = if counted {
        let record = counts.entry(tag.to_owned()).or_default();
        if record.count > 0 && !record.is_fresh() {
            // Strikes that have lapsed do not carry forward: one new failure
            // after a quiet week starts a fresh set rather than parking the
            // release on the spot.
            record.count = 0;
        }
        record.count = record.count.saturating_add(1);
        record.last_at = chrono::Utc::now().to_rfc3339();
        record.count
    } else {
        // A network, quota or timing failure says nothing about the package, so
        // it neither adds a strike nor keeps an old set of strikes alive.
        counts.get(tag).map(|record| record.count).unwrap_or(0)
    };
    let stored = serde_json::to_string(&counts).unwrap_or_else(|_| "{}".to_owned());
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.install_failures',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(&stored)
        .execute(&state.db)
        .await
    {
        tracing::warn!("could not record the failed update: {e:#}");
    }
    let error = json!({
        "tag": tag,
        // The page prints versions with its own "v"; handing it the bare version
        // is what keeps "vv1.5.0" from ever appearing.
        "version": tag.trim_start_matches('v'),
        "message": message,
        "attempts": attempts,
        "counted": counted,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.install_error',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(error.to_string())
        .execute(&state.db)
        .await
    {
        tracing::warn!("could not record the failed update: {e:#}");
    }
    overlay_failures(state, &counts).await;
    state.updates.write().await["install_error"] = error;
}

/// An explicit install attempt restarts the count for that release, so an
/// administrator who retries on purpose is not blocked by earlier failures.
async fn clear_install_failure(state: &AppState, tag: &str) {
    let mut counts = install_failures(state).await;
    counts.remove(tag);
    let stored = serde_json::to_string(&counts).unwrap_or_else(|_| "{}".to_owned());
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.install_failures',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(&stored)
        .execute(&state.db)
        .await
    {
        tracing::warn!("could not clear the failed update: {e:#}");
    }
    if let Err(e) = sqlx::query("DELETE FROM app_settings WHERE key='updates.install_error'")
        .execute(&state.db)
        .await
    {
        tracing::warn!("could not clear the failed update: {e:#}");
    }
    overlay_failures(state, &counts).await;
    state.updates.write().await["install_error"] = Value::Null;
}

/// Puts the current failure counts back onto the cached release list, so the
/// page never shows a stale count while a retry has already reset it.
async fn overlay_failures(
    state: &AppState,
    counts: &std::collections::HashMap<String, FailureCount>,
) {
    let mut value = state.updates.write().await;
    if let Some(releases) = value["releases"].as_array_mut() {
        for release in releases.iter_mut() {
            release["install_failures"] =
                release_failures(counts, release["tag"].as_str().unwrap_or(""));
        }
    }
}

async fn policy(state: &AppState) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM app_settings WHERE key='updates.policy'")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
}

async fn channel(state: &AppState) -> String {
    sqlx::query_scalar("SELECT value FROM app_settings WHERE key='updates.channel'")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "stable".into())
}

/// A release the stable channel accepts: GitHub's flag and the version string
/// must both say it is not a pre-release.
fn is_stable(release: &Value) -> bool {
    release["prerelease"].is_boolean()
        && release["prerelease"] == false
        && release["version"]
            .as_str()
            .and_then(|version| semver::Version::parse(version).ok())
            .is_some_and(|version| version.pre.is_empty())
}

/// The single channel rule the UI and the installer share, so the release an
/// instance is shown is always the release it is allowed to install.
fn on_channel(release: &Value, beta: bool) -> bool {
    release["newer"] == true && release["prerelease"].is_boolean() && (beta || is_stable(release))
}

async fn latest_available(state: &AppState) -> Option<String> {
    let failed = tokio::fs::read_to_string(state.data_dir.join("failed-update.txt"))
        .await
        .ok();
    let beta = channel(state).await == "beta";
    let failures = install_failures(state).await;
    let updates = state.updates.read().await;
    automatic_release(
        updates["releases"].as_array()?,
        failed.as_deref(),
        beta,
        &failures,
    )
}

fn automatic_release(
    releases: &[Value],
    failed: Option<&str>,
    beta: bool,
    failures: &std::collections::HashMap<String, FailureCount>,
) -> Option<String> {
    // Only the newest release on the channel is considered: installing an older
    // one than the page is offering would leave the instance and the UI telling
    // different stories, and a release that keeps failing here is left alone.
    releases
        .iter()
        .find(|release| on_channel(release, beta))
        .filter(|release| {
            let tag = release["tag"].as_str().unwrap_or("");
            release["supported"] == true
                && failed != release["tag"].as_str()
                && !failures.get(tag).is_some_and(FailureCount::parked)
        })
        .and_then(|release| release["tag"].as_str().map(str::to_owned))
}

/// An install the administrator asked for while automations were running. The
/// release metadata travels with the request, so the update still happens after
/// a restart even when GitHub is unreachable once the runs finish.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Pending {
    tag: String,
    release: Value,
    attempts: u32,
    next_try: String,
}

async fn pending_install(state: &AppState) -> Option<Pending> {
    queued_install(state).await.map(|(_, pending)| pending)
}

/// The queued install together with the exact text stored for it. The text is
/// the queue's identity: writing back only succeeds while nothing else has
/// cancelled or replaced the request in the meantime.
async fn queued_install(state: &AppState) -> Option<(String, Pending)> {
    let stored = sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_settings WHERE key='updates.pending_install'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;
    Some(stored.clone()).zip(serde_json::from_str(&stored).ok())
}

async fn save_pending(state: &AppState, pending: &Pending) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.pending_install',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(serde_json::to_string(pending)?)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Keeps a queued install for another attempt, but only if it is still the
/// request this worker read. An administrator who cancelled it, or queued a
/// different release, wins over a retry that was already in flight.
async fn resave_pending(state: &AppState, stored: &str, pending: &Pending) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "UPDATE app_settings SET value=? WHERE key='updates.pending_install' AND value=?",
    )
    .bind(serde_json::to_string(pending)?)
    .bind(stored)
    .execute(&state.db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Drops the queued install this worker read, and only that one.
async fn clear_queued(state: &AppState, stored: &str) -> bool {
    match sqlx::query("DELETE FROM app_settings WHERE key='updates.pending_install' AND value=?")
        .bind(stored)
        .execute(&state.db)
        .await
    {
        Ok(result) => result.rows_affected() == 1,
        Err(e) => {
            tracing::warn!("could not clear the queued update: {e:#}");
            false
        }
    }
}

async fn clear_pending_install(state: &AppState) {
    if let Err(e) = sqlx::query("DELETE FROM app_settings WHERE key='updates.pending_install'")
        .execute(&state.db)
        .await
    {
        tracing::warn!("could not clear the queued update: {e:#}");
    }
}

/// Why a queued install should no longer happen, or `None` to go ahead with it.
/// A queued request was accepted against the releases and the channel of the
/// moment it was pressed; if the image was upgraded underneath it, or the channel
/// changed while automations finished, installing it anyway would be a decision
/// nobody made.
fn queued_release_stale(release: &Value, beta: bool) -> Option<&'static str> {
    let running = semver::Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
    let wanted = release["version"]
        .as_str()
        .and_then(|version| semver::Version::parse(version).ok())
        .or_else(|| {
            release["tag"]
                .as_str()
                .and_then(|tag| semver::Version::parse(tag.trim_start_matches('v')).ok())
        })?;
    if wanted <= running {
        return Some("this instance already runs that release or a newer one");
    }
    if !(beta || is_stable(release)) {
        return Some("the release is no longer on this channel");
    }
    if release["supported"] != true {
        return Some("the release package is not installable here");
    }
    None
}

/// Installs whatever update is due: first one the administrator queued by hand,
/// then the newest release when the automatic policy asks for it.
async fn install_due_update(state: &AppState) -> Result<AutomaticOutcome> {
    let active_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status='running'")
        .fetch_one(&state.db)
        .await?;
    if let Some((stored, pending)) = queued_install(state).await {
        let beta = channel(state).await == "beta";
        if let Some(reason) = queued_release_stale(&pending.release, beta) {
            tracing::warn!("dropping the queued update to {}: {reason}", pending.tag);
            clear_queued(state, &stored).await;
            return Ok(AutomaticOutcome::Idle);
        }
        let due = chrono::DateTime::parse_from_rfc3339(&pending.next_try)
            .map(|when| chrono::Utc::now() >= when.with_timezone(&chrono::Utc))
            .unwrap_or(true);
        if active_runs > 0 || !due {
            tracing::debug!(
                "the queued update to {} waits for a quiet moment",
                pending.tag
            );
            return Ok(AutomaticOutcome::Deferred);
        }
        clear_install_failure(state, &pending.tag).await;
        let intent = Intent::Queued { claim: &stored };
        match stage_release(state, &pending.release, &intent).await {
            Ok(()) => {
                // stage_release cleared the request under the same lock that
                // stops a cancellation from being lost.
                return Ok(AutomaticOutcome::Restarting);
            }
            Err(e) => {
                if withdrawn(&e) {
                    // The administrator cancelled while the package was being
                    // fetched. Nothing failed and nothing is waiting to retry.
                    tracing::info!(
                        "the queued update to {} was cancelled before it installed",
                        pending.tag
                    );
                    return Ok(AutomaticOutcome::Idle);
                }
                let counted = fatal(&e);
                record_install_failure(state, &pending.tag, &format!("{e:#}"), counted).await;
                if counted {
                    // A package that cannot install here will not improve by
                    // asking again; the administrator sees why in Settings.
                    clear_queued(state, &stored).await;
                } else {
                    let mut retry = pending.clone();
                    retry.attempts = retry.attempts.saturating_add(1);
                    retry.next_try = (chrono::Utc::now()
                        + chrono::Duration::seconds(60 * i64::from(retry.attempts.max(1))))
                    .to_rfc3339();
                    match resave_pending(state, &stored, &retry).await {
                        Ok(true) => {}
                        Ok(false) => tracing::info!(
                            "the queued update to {} was cancelled or replaced while it ran; not restoring it",
                            pending.tag
                        ),
                        Err(e) => tracing::warn!("could not keep the queued update: {e:#}"),
                    }
                }
                return Err(e);
            }
        }
    }
    if policy(state).await.as_deref() != Some("automatic") {
        return Ok(AutomaticOutcome::Idle);
    }
    let Some(tag) = latest_available(state).await else {
        return Ok(AutomaticOutcome::Idle);
    };
    if active_runs > 0 {
        tracing::info!(
            "deferring the automatic update to {tag} while {active_runs} automation run(s) are active"
        );
        return Ok(AutomaticOutcome::Deferred);
    }
    let Some(release) = cached_release(state, &tag).await else {
        return Ok(AutomaticOutcome::Idle);
    };
    let intent = Intent::Automatic;
    if let Err(e) = stage_release(state, &release, &intent).await {
        if withdrawn(&e) {
            // The administrator asked for another release while this one was
            // being prepared; their request is the one to carry out next.
            tracing::info!("the automatic update gives way to a request made by hand: {e:#}");
            return Ok(AutomaticOutcome::Idle);
        }
        record_install_failure(state, &tag, &format!("{e:#}"), fatal(&e)).await;
        return Err(e);
    }
    Ok(AutomaticOutcome::Restarting)
}

/// The stored copy of a release the updater already fetched from GitHub.
async fn cached_release(state: &AppState, tag: &str) -> Option<Value> {
    state.updates.read().await["releases"]
        .as_array()
        .and_then(|rs| rs.iter().find(|r| r["tag"] == tag).cloned())
}

async fn install(state: &AppState, tag: &str) -> Result<()> {
    let beta = channel(state).await == "beta";
    let release = cached_release(state, tag)
        .await
        .filter(|release| release["newer"] == true && on_channel(release, beta))
        .with_context(|| {
            format!(
                "release {tag} is not a newer release on the {} channel",
                if beta { "beta" } else { "stable" }
            )
        })?;
    anyhow::ensure!(
        release["supported"] == true,
        "release {tag} cannot be installed here: {}",
        release["unsupported_reason"]
            .as_str()
            .unwrap_or("no usable update package for this platform")
    );
    stage_release(state, &release, &Intent::Direct { tag }).await
}

/// What authorized a staging, and how it is confirmed again under the lock that
/// cancellations, channel changes and new requests also take. Fetching a package
/// takes minutes; the decision about whether anyone still wants it does not, so
/// the two are separated: `stage_release` confirms the intent, fetches, and then
/// confirms it once more before anything on disk changes.
enum Intent<'a> {
    /// The automatic policy asked for the newest release on the channel. It
    /// steps aside for anything the administrator asked for by hand.
    Automatic,
    /// A queued request, whose exact stored row is the claim.
    Queued { claim: &'a str },
    /// A button pressed just now, with no automations running.
    Direct { tag: &'a str },
}

fn withdrawal(reason: &str) -> anyhow::Error {
    anyhow::anyhow!("that update is no longer queued: {reason}").context(Withdrawn)
}

/// Whether the thing that authorized this staging still stands. Call it while
/// holding `update_lock`, so a cancellation or a channel change either happened
/// before this - and is seen - or is still waiting behind it.
async fn still_wanted(state: &AppState, intent: &Intent<'_>) -> Result<()> {
    let queued = queued_install(state).await;
    match intent {
        Intent::Queued { claim } => {
            let (stored, pending) = match queued {
                Some(queued) => queued,
                None => return Err(withdrawal("it was cancelled")),
            };
            if stored != *claim {
                return Err(withdrawal("a different release was asked for since"));
            }
            let beta = channel(state).await == "beta";
            if let Some(reason) = queued_release_stale(&pending.release, beta) {
                return Err(withdrawal(reason));
            }
        }
        Intent::Automatic => {
            if let Some((_, pending)) = &queued {
                return Err(withdrawal(&format!(
                    "{} was asked for by hand",
                    pending.tag
                )));
            }
        }
        Intent::Direct { tag } => {
            if let Some((_, pending)) = &queued {
                if pending.tag != *tag {
                    return Err(withdrawal(&format!(
                        "{} was asked for by hand",
                        pending.tag
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Downloads, verifies and stages one release, then asks the service to
/// restart. Every rule the release list uses to call a package `supported` is
/// enforced here too, so an offered install is always an installable one.
async fn stage_release(state: &AppState, release: &Value, intent: &Intent<'_>) -> Result<()> {
    {
        let _gate = state.update_lock.lock().await;
        still_wanted(state, intent).await?;
    }
    let tag = release["tag"].as_str().unwrap_or("");
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
        .context("release package URL is missing")
        .context(Fatal)?;
    let digest = release["asset_digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .with_context(|| "GitHub release metadata does not provide a SHA-256 checksum")
        .context(Fatal)?;
    let size = release["asset_size"].as_u64().unwrap_or(0);
    anyhow::ensure!(
        size > 0 && size <= MAX_ARCHIVE as u64,
        "release package size is invalid"
    );
    let parsed = url::Url::parse(url).context("release package URL is unusable")?;
    anyhow::ensure!(
        parsed.scheme() == "https" && parsed.host_str() == Some("github.com"),
        "release package must be hosted on GitHub over HTTPS"
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
    let actual = hex::encode(Sha256::digest(&archive));
    if !actual.eq_ignore_ascii_case(digest) {
        return Err(
            anyhow::anyhow!("release package checksum does not match GitHub metadata")
                .context(Fatal),
        );
    }

    // Nothing has touched `/data` yet. Under the same lock a cancellation or a
    // channel change takes, decide once more whether anyone still wants this.
    let _update_guard = state.update_lock.lock().await;
    still_wanted(state, intent).await?;
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
    let data_dir = state.data_dir.clone();
    // extract_release marks what the package got wrong; a job that never
    // reports back is this process's problem, not the release's, so it is not
    // counted against the release.
    let dir = tokio::task::spawn_blocking(move || extract_release(&data_dir, &tag_safe, &archive))
        .await
        .map_err(|e| anyhow::anyhow!("unpacking the release package was interrupted: {e}"))??;
    let _ = tokio::fs::remove_file(state.data_dir.join("failed-update.txt")).await;
    let descriptor = json!({"version":release["version"],"tag":tag,"path":dir.to_string_lossy(),"attempts":0,"confirmed":false});
    let tmp = state.data_dir.join("active-update.json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(&descriptor)?).await?;
    tokio::fs::rename(tmp, state.data_dir.join("active-update.json")).await?;
    // The promise is kept, so the request that authorized it goes away here,
    // under the same lock, rather than by whoever asked.
    match intent {
        Intent::Queued { claim } => {
            clear_queued(state, claim).await;
        }
        Intent::Direct { tag } => {
            if let Some((stored, pending)) = queued_install(state).await {
                if pending.tag == *tag {
                    clear_queued(state, &stored).await;
                }
            }
        }
        Intent::Automatic => {}
    }
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
    // Two kinds of failure meet here. Something wrong with the package itself
    // will be wrong on every attempt, so it is marked fatal and the updater
    // stops chasing it. A failed write is the disk's fault, and stays retryable.
    macro_rules! package_ensure {
        ($cond:expr, $($arg:tt)+) => {
            if !$cond {
                return Err(anyhow::anyhow!($($arg)+).context(Fatal));
            }
        };
    }
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
    let entries = archive
        .entries()
        .map_err(|e| anyhow::Error::from(e).context(Fatal))?;
    for (total_entries, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|e| anyhow::Error::from(e).context(Fatal))?;
        package_ensure!(
            total_entries < 20_000,
            "release package contains too many files"
        );
        let path = entry
            .path()
            .map_err(|e| anyhow::Error::from(e).context(Fatal))?
            .into_owned();
        package_ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_))),
            "release archive has an unsafe path"
        );
        package_ensure!(
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
        package_ensure!(
            parts
                .first()
                .is_some_and(|p| matches!(p.as_ref(), "diffrook" | "web" | "LICENSE" | "licenses")),
            "unexpected file in release archive"
        );
        if parts[0] == "diffrook" || parts[0] == "LICENSE" {
            package_ensure!(parts.len() == 1, "unexpected root archive path");
            package_ensure!(
                entry.header().entry_type().is_file(),
                "root archive entry must be a regular file"
            );
            if parts[0] == "diffrook" {
                binary = true;
            }
        }
        if parts.len() == 2
            && parts[0] == "web"
            && parts[1] == "index.html"
            && entry.header().entry_type().is_file()
        {
            web_index = true;
        }
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(stage.join(&path))?;
            continue;
        }
        total = total.saturating_add(entry.size());
        package_ensure!(
            total <= MAX_EXTRACTED,
            "release expands beyond the size limit"
        );
        let out = stage.join(&path);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Reading and writing are kept apart so a corrupt stream - the package's
        // fault, and fatal - is not confused with a failing disk, which is not.
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut body)
            .map_err(|e| anyhow::Error::from(e).context(Fatal))?;
        std::fs::write(out, &body)?;
    }
    package_ensure!(
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

/// The release this instance should be shown: the newest one on its channel.
/// Shares `on_channel` with the installer, so the overview never offers a
/// release the updater would refuse to install.
fn offered_release(releases: &[Value], beta: bool) -> Option<&Value> {
    releases.iter().find(|r| on_channel(r, beta))
}

fn offer_value(release: Option<&Value>) -> Value {
    match release {
        Some(r) => json!({
            "tag": r["tag"],
            "version": r["version"],
            "prerelease": r["prerelease"],
            "supported": r["supported"],
            "unsupported_reason": r["unsupported_reason"],
            "rolled_back": r["rolled_back"],
            "install_failures": r["install_failures"],
            "published_at": r["published_at"],
            "url": r["url"],
        }),
        None => Value::Null,
    }
}

fn offer_in(state_updates: &Value, beta: bool) -> Value {
    offer_value(offered_release(
        state_updates["releases"]
            .as_array()
            .map_or(&[][..], |a| a.as_slice()),
        beta,
    ))
}

pub async fn api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, false).await {
        return r;
    }
    let mut value = state.updates.read().await.clone();
    let beta = channel(&state).await == "beta";
    // The counts and the last failure come from the database, so the page never
    // shows a stale number after a retry has already reset it.
    let counts = install_failures(&state).await;
    if let Some(releases) = value["releases"].as_array_mut() {
        for release in releases.iter_mut() {
            release["install_failures"] =
                release_failures(&counts, release["tag"].as_str().unwrap_or(""));
        }
    }
    value["install_error"] = install_error(&state).await;
    value["policy"] = json!(policy(&state).await.unwrap_or_else(|| "manual".into()));
    value["channel"] = json!(channel(&state).await);
    value["latest"] = offer_in(&value, beta);
    value["pending_install"] = json!(pending_install(&state).await.map(|pending| pending.tag));
    value["pending_version"] = json!(pending_install(&state).await.map(|pending| {
        pending.release["version"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| pending.tag.trim_start_matches('v').to_owned())
    }));
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
    automatic_response(&state, install_due_update(&state).await, "mode", mode, None).await
}

/// Drops a queued install the chosen channel no longer offers, so switching from
/// beta back to stable cannot leave a pre-release waiting to be installed.
async fn drop_stale_queue(state: &AppState, beta: bool) -> Option<String> {
    // Same gate as the installer and the cancel endpoint: the queue is decided
    // in one place at a time.
    let _gate = state.update_lock.lock().await;
    let (stored, pending) = queued_install(state).await?;
    queued_release_stale(&pending.release, beta)?;
    if clear_queued(state, &stored).await {
        tracing::info!(
            "the queued update to {} was dropped: it is not on the chosen channel",
            pending.tag
        );
        return Some(pending.tag);
    }
    None
}

/// Turns an automatic-update attempt into the JSON both settings endpoints
/// return, so the UI can tell "installing now" from "waiting for a free moment".
async fn automatic_response(
    state: &AppState,
    result: Result<AutomaticOutcome>,
    field: &str,
    value: &str,
    dropped: Option<&str>,
) -> Response {
    // The setting itself was already saved, so a failed immediate install is
    // reported as part of a successful change rather than a rejected request.
    let outcome = match result {
        Ok(outcome) => Some(outcome),
        Err(e) => {
            tracing::warn!("update after changing {field} failed: {e:#}");
            None
        }
    };
    Json(json!({
        field: value,
        "restarting": matches!(outcome, Some(AutomaticOutcome::Restarting)),
        "deferred": matches!(outcome, Some(AutomaticOutcome::Deferred)),
        "install_error": state.updates.read().await["install_error"],
        "dropped_pending": dropped,
    }))
    .into_response()
}

pub async fn set_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, true).await {
        return r;
    }
    let Some(channel @ ("stable" | "beta")) = body["channel"].as_str() else {
        return error(StatusCode::BAD_REQUEST, "Choose stable or beta releases");
    };
    if let Err(e) = sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.channel',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(channel).execute(&state.db).await { return error(StatusCode::INTERNAL_SERVER_ERROR, e); }
    // A queued install was accepted against the old channel. If the new one does
    // not offer that release, saying so now is better than letting it arrive later.
    let dropped = drop_stale_queue(&state, channel == "beta").await;
    automatic_response(
        &state,
        install_due_update(&state).await,
        "channel",
        channel,
        dropped.as_deref(),
    )
    .await
}

/// Runs a release check on demand so an administrator can confirm the GitHub
/// release feed without waiting for the next scheduled pass. Repeated clicks are
/// throttled, because every one of them is a call to GitHub's rate-limited API.
/// Clears the manual release-check flag when the request finishes, however it
/// finishes, so a failed check cannot leave the button disabled forever.
struct CheckGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for CheckGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub async fn check_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, true).await {
        return r;
    }
    // Two administrators clicking at the same moment share one GitHub request.
    if state
        .release_check_busy
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "A release check is already running.",
        );
    }
    let _check = CheckGuard(state.release_check_busy.clone());
    // The cooldown counts attempts, not successes: a check that GitHub refused
    // spent the same quota as one that worked.
    let last_attempt: Option<String> =
        sqlx::query_scalar("SELECT value FROM app_settings WHERE key='updates.last_attempt'")
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let updates_now = state.updates.read().await.clone();
    let hinted = updates_now["retry_after"].as_u64().unwrap_or(0) as i64;
    if let Some(stored) = last_attempt.as_deref() {
        if let Ok(when) = chrono::DateTime::parse_from_rfc3339(stored) {
            let elapsed = chrono::Utc::now()
                .signed_duration_since(when.with_timezone(&chrono::Utc))
                .num_seconds();
            let wait = MANUAL_CHECK_COOLDOWN.max(hinted).saturating_sub(elapsed);
            if wait > 0 {
                let reason = if hinted > 0 && hinted >= elapsed {
                    "GitHub is limiting these checks"
                } else {
                    "A check already ran"
                };
                return error(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("{reason} moments ago. Try again in {wait} seconds."),
                );
            }
        }
    }
    if let Err(e) = refresh(&state).await {
        let updates = state.updates.read().await;
        let wait = updates["retry_after"].as_u64();
        return error(
            StatusCode::BAD_GATEWAY,
            match wait {
                Some(seconds) => format!(
                    "GitHub release check failed: {e:#}. The next automatic retry waits {seconds} seconds."
                ),
                None => format!("GitHub release check failed: {e:#}"),
            },
        );
    }
    let outcome = install_due_update(&state).await;
    let updates = state.updates.read().await.clone();
    let beta = channel(&state).await == "beta";
    let (restarting, deferred, install_error) = match &outcome {
        Ok(AutomaticOutcome::Restarting) => (true, false, None),
        Ok(AutomaticOutcome::Deferred) => (false, true, None),
        Ok(AutomaticOutcome::Idle) => (false, false, None),
        Err(e) => (false, false, Some(format!("{e:#}"))),
    };
    Json(json!({
        "ok": true,
        "current_version": updates["current_version"],
        "checked_at": updates["checked_at"],
        "latest": offer_in(&updates, beta),
        "restarting": restarting,
        "deferred": deferred,
        "install_error": install_error,
    }))
    .into_response()
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
    let active_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status='running'")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    // An explicit request always restarts the failure counter for that release.
    clear_install_failure(&state, tag).await;
    if active_runs > 0 {
        // The interface promises an update waits for running automations, so the
        // request is recorded, with the release metadata that makes it possible,
        // and carried out when they finish - even if the container restarts in
        // between and GitHub is unreachable afterwards.
        let beta = channel(&state).await == "beta";
        let Some(release) = cached_release(&state, tag).await else {
            return error(
                StatusCode::BAD_REQUEST,
                format!("release {tag} is not a release this instance has checked"),
            );
        };
        if release["newer"] != true || !on_channel(&release, beta) {
            return error(
                StatusCode::BAD_REQUEST,
                format!("release {tag} is not a newer release on this channel"),
            );
        }
        if release["supported"] != true {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                    "release {tag} cannot be installed here: {}",
                    release["unsupported_reason"]
                        .as_str()
                        .unwrap_or("no usable update package for this platform")
                ),
            );
        }
        let pending = Pending {
            tag: tag.to_owned(),
            release,
            attempts: 0,
            next_try: chrono::Utc::now().to_rfc3339(),
        };
        // Same gate as the installer and the cancel endpoint, so queuing cannot
        // land between an installer checking the queue and staging it.
        let _gate = state.update_lock.lock().await;
        if let Err(e) = save_pending(&state, &pending).await {
            return error(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
        return Json(json!({"ok":true,"queued":true,"restarting":false})).into_response();
    }
    match install(&state, tag).await {
        Ok(()) => {
            // The request for this release has now been carried out; anything
            // queued for a different release stays exactly as it was.
            if let Some((stored, pending)) = queued_install(&state).await {
                if pending.tag == tag {
                    clear_queued(&state, &stored).await;
                }
            }
            Json(json!({"ok":true,"restarting":true})).into_response()
        }
        // A failed attempt leaves the queue alone. Dropping a request that
        // belongs to another release would lose an instruction nobody withdrew.
        Err(e) => error(StatusCode::BAD_REQUEST, format!("Update failed: {e:#}")),
    }
}

/// Gives up on a queued install, so the administrator can change their mind.
pub async fn cancel_pending(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = crate::routes::guard(&state, &headers, true).await {
        return r;
    }
    // Taken in the same order as the installer: cancelling cannot slip between
    // the installer checking the queue and it staging the package.
    let _gate = state.update_lock.lock().await;
    if pending_install(&state).await.is_none() {
        return error(StatusCode::NOT_FOUND, "No update is queued");
    }
    clear_pending_install(&state).await;
    Json(json!({"ok": true, "cancelled": true})).into_response()
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
    #[test]
    fn offered_release_follows_the_channel() {
        let releases = vec![
            json!({"tag":"v2.0.0-beta.1","version":"2.0.0-beta.1","newer":true,"prerelease":true,"supported":true}),
            json!({"tag":"v1.9.0","version":"1.9.0","newer":true,"prerelease":false,"supported":true}),
            json!({"tag":"v1.0.1","version":"1.0.1","newer":false,"prerelease":false,"supported":true}),
        ];
        assert_eq!(
            offered_release(&releases, false).and_then(|r| r["tag"].as_str()),
            Some("v1.9.0")
        );
        assert_eq!(
            offered_release(&releases, true).and_then(|r| r["tag"].as_str()),
            Some("v2.0.0-beta.1")
        );
        assert!(offered_release(&releases[2..], false).is_none());
        assert_eq!(offer_value(None), Value::Null);
    }

    #[test]
    fn stable_channel_rejects_a_prerelease_github_left_unflagged() {
        // GitHub's checkbox and the version string must both say stable, or the
        // stable channel would offer what automatic updates refuse to install.
        let releases = vec![json!({
            "tag": "v1.9.0-beta.1",
            "version": "1.9.0-beta.1",
            "newer": true,
            "prerelease": false,
            "supported": true,
        })];
        assert!(offered_release(&releases, false).is_none());
        assert!(
            automatic_release(&releases, None, false, &std::collections::HashMap::new()).is_none()
        );
        assert_eq!(
            offered_release(&releases, true).and_then(|r| r["tag"].as_str()),
            Some("v1.9.0-beta.1")
        );
        assert_eq!(
            automatic_release(&releases, None, true, &std::collections::HashMap::new()).as_deref(),
            Some("v1.9.0-beta.1")
        );
    }

    #[test]
    fn a_shown_offer_carries_the_facts_the_installer_uses() {
        let releases = vec![json!({
            "tag": "v2.0.0",
            "version": "2.0.0",
            "newer": true,
            "prerelease": false,
            "supported": false,
            "rolled_back": true,
            "published_at": "2026-11-04T10:00:00Z",
            "url": "https://github.com/Alexandre1116/diffrook/releases/tag/v2.0.0",
        })];
        let offer = offer_value(offered_release(&releases, false));
        assert_eq!(offer["tag"], "v2.0.0");
        assert_eq!(
            offer["supported"], false,
            "the UI must say why it cannot install"
        );
        assert_eq!(
            offer["rolled_back"], true,
            "a rolled-back release is disclosed"
        );
    }

    #[test]
    fn rate_limit_headers_set_the_retry_wait() {
        use reqwest::header::HeaderMap;
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "37".parse().unwrap());
        assert_eq!(
            retry_after(StatusCode::INTERNAL_SERVER_ERROR, &headers),
            Some(Duration::from_secs(37))
        );
        let mut headers = HeaderMap::new();
        let reset = chrono::Utc::now().timestamp() + 120;
        headers.insert("x-ratelimit-reset", reset.to_string().parse().unwrap());
        headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
        let wait = retry_after(StatusCode::FORBIDDEN, &headers).unwrap();
        assert!((Duration::from_secs(115)..=Duration::from_secs(126)).contains(&wait));
        // A 502 carries the same rate-limit headers on almost every response.
        // Treating them as a requested wait would silence checks for an hour.
        assert_eq!(retry_after(StatusCode::BAD_GATEWAY, &headers), None);
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", reset.to_string().parse().unwrap());
        headers.insert("x-ratelimit-remaining", "41".parse().unwrap());
        assert_eq!(retry_after(StatusCode::FORBIDDEN, &headers), None);
        assert_eq!(
            retry_after(StatusCode::OK, &HeaderMap::new()),
            None,
            "no headers, no wait"
        );
    }

    #[test]
    fn a_release_is_only_offered_when_the_installer_can_use_it() {
        let good = json!({
            "browser_download_url": "https://github.com/Alexandre1116/diffrook/releases/download/v2.0.0/diffrook-linux-x86_64.tar.gz",
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "size": 40_000_000,
        });
        assert_eq!(package_problem(Some(&good)), None);
        assert!(package_installable(
            good["browser_download_url"].as_str(),
            good["digest"].as_str(),
            good["size"].as_u64().unwrap()
        ));
        // No checksum: the installer would download 40 MB and then refuse it.
        let no_digest =
            json!({"browser_download_url": good["browser_download_url"], "size": 40_000_000});
        assert_eq!(
            package_problem(Some(&no_digest)),
            Some("GitHub publishes no checksum for this package")
        );
        let wrong_host = json!({"browser_download_url": "https://example.com/pkg.tar.gz", "digest": good["digest"], "size": 40_000_000});
        assert_eq!(
            package_problem(Some(&wrong_host)),
            Some("the update package for this platform cannot be installed here")
        );
        let oversized = json!({"browser_download_url": good["browser_download_url"], "digest": good["digest"], "size": MAX_ARCHIVE as u64 + 1});
        assert_eq!(
            package_problem(Some(&oversized)),
            Some("the update package for this platform cannot be installed here")
        );
        assert_eq!(
            package_problem(None),
            Some("no update package is published for this platform")
        );
    }

    #[test]
    fn install_failures_expire_and_only_count_real_rejections() {
        let fresh = FailureCount {
            count: INSTALL_FAILURE_LIMIT,
            last_at: chrono::Utc::now().to_rfc3339(),
        };
        assert!(fresh.parked(), "three failures in a row stop the updater");
        let stale = FailureCount {
            count: INSTALL_FAILURE_LIMIT,
            last_at: (chrono::Utc::now() - chrono::Duration::hours(7)).to_rfc3339(),
        };
        assert!(
            !stale.parked(),
            "a week of network trouble must not park the instance forever"
        );
        let few = FailureCount {
            count: INSTALL_FAILURE_LIMIT - 1,
            last_at: chrono::Utc::now().to_rfc3339(),
        };
        assert!(!few.parked());
        assert!(
            !FailureCount::default().parked(),
            "a release nobody tried has no strikes"
        );
    }

    #[tokio::test]
    async fn a_queued_install_carries_its_own_release_metadata() {
        // The queued request must survive a restart and a GitHub outage: it holds
        // everything the installer needs, not just the tag.
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let release = json!({
            "tag": "v9.9.9", "version": "9.9.9", "supported": true,
            "asset_url": "https://github.com/Alexandre1116/diffrook/releases/download/v9.9.9/diffrook-linux-x86_64.tar.gz",
            "asset_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "asset_size": 1234,
        });
        let pending = Pending {
            tag: "v9.9.9".into(),
            release: release.clone(),
            attempts: 0,
            next_try: chrono::Utc::now().to_rfc3339(),
        };
        save_pending(&state, &pending).await.unwrap();
        let stored = pending_install(&state)
            .await
            .expect("queued request persisted");
        assert_eq!(stored.tag, "v9.9.9");
        assert_eq!(stored.release["asset_url"], release["asset_url"]);
        assert_eq!(
            stored.release["asset_digest"], release["asset_digest"],
            "the digest travels with the request, so the update can verify after a restart"
        );
        clear_pending_install(&state).await;
        assert!(pending_install(&state).await.is_none());
    }

    #[tokio::test]
    async fn a_cancelled_or_replaced_queue_is_never_restored_by_a_retry() {
        // The worker that lost the race must not put back a request nobody wants.
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let release = json!({
            "tag": "v9.9.9", "version": "9.9.9", "prerelease": false, "supported": true,
            "asset_url": "https://github.com/Alexandre1116/diffrook/releases/download/v9.9.9/diffrook-linux-x86_64.tar.gz",
            "asset_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "asset_size": 1234,
        });
        let pending = Pending {
            tag: "v9.9.9".into(),
            release: release.clone(),
            attempts: 0,
            next_try: chrono::Utc::now().to_rfc3339(),
        };
        save_pending(&state, &pending).await.unwrap();
        let (read, _) = queued_install(&state).await.unwrap();

        clear_pending_install(&state).await;
        assert!(
            !resave_pending(&state, &read, &pending).await.unwrap(),
            "a retry must not resurrect a cancelled request"
        );
        assert!(pending_install(&state).await.is_none());

        let other = Pending {
            tag: "v8.8.8".into(),
            release,
            attempts: 0,
            next_try: chrono::Utc::now().to_rfc3339(),
        };
        save_pending(&state, &other).await.unwrap();
        assert!(
            !clear_queued(&state, &read).await,
            "an out-of-date request deletes nothing that came after it"
        );
        assert_eq!(pending_install(&state).await.unwrap().tag, "v8.8.8");
        let (current, _) = queued_install(&state).await.unwrap();
        assert!(clear_queued(&state, &current).await);
        assert!(pending_install(&state).await.is_none());
    }

    #[test]
    fn a_queued_install_is_dropped_when_it_stops_being_an_offered_release() {
        let beta =
            json!({"tag":"v9.9.9-rc.1","version":"9.9.9-rc.1","prerelease":true,"supported":true});
        assert!(
            queued_release_stale(&beta, true).is_none(),
            "a pre-release queued on the beta channel stays queued"
        );
        assert_eq!(
            queued_release_stale(&beta, false),
            Some("the release is no longer on this channel"),
            "switching back to stable must not leave a pre-release queued"
        );
        let old = json!({"tag":"v0.2.0","version":"0.2.0","prerelease":false,"supported":true});
        assert_eq!(
            queued_release_stale(&old, false),
            Some("this instance already runs that release or a newer one"),
            "an image upgrade overtakes the queue"
        );
        let unusable =
            json!({"tag":"v99.0.0","version":"99.0.0","prerelease":false,"supported":false});
        assert_eq!(
            queued_release_stale(&unusable, false),
            Some("the release package is not installable here")
        );
    }

    #[tokio::test]
    async fn an_install_withdrawn_before_it_starts_is_not_a_failed_package() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let release = json!({
            "tag": "v9.9.9", "version": "9.9.9", "prerelease": false, "supported": true,
            "asset_url": "https://github.com/Alexandre1116/diffrook/releases/download/v9.9.9/diffrook-linux-x86_64.tar.gz",
            "asset_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "asset_size": 1234,
        });
        save_pending(
            &state,
            &Pending {
                tag: "v9.9.9".into(),
                release: release.clone(),
                attempts: 0,
                next_try: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();
        let (claim, pending) = queued_install(&state).await.unwrap();

        // The administrator withdraws the request while the worker is on its way.
        clear_pending_install(&state).await;
        let intent = Intent::Queued { claim: &claim };
        let error = stage_release(&state, &pending.release, &intent)
            .await
            .unwrap_err();
        assert!(withdrawn(&error), "{error:#}");
        assert!(
            install_failures(&state).await.is_empty(),
            "a withdrawn install is not a strike against the release"
        );
        assert!(install_error(&state).await.is_null());
        assert!(
            !*state.restart.subscribe().borrow(),
            "nothing was staged, so nothing restarts"
        );

        // The same for a request replaced by one for another release.
        save_pending(
            &state,
            &Pending {
                tag: "v8.8.8".into(),
                release: release.clone(),
                attempts: 0,
                next_try: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();
        let intent = Intent::Queued { claim: &claim };
        let error = stage_release(&state, &release, &intent).await.unwrap_err();
        assert!(withdrawn(&error), "an out-of-date claim installs nothing");
    }

    #[tokio::test]
    async fn the_channel_is_checked_again_where_the_queue_is_decided() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let release = json!({
            "tag": "v9.9.9-rc.1", "version": "9.9.9-rc.1", "prerelease": true, "supported": true,
            "asset_url": "https://github.com/Alexandre1116/diffrook/releases/download/v9.9.9-rc.1/diffrook-linux-x86_64.tar.gz",
            "asset_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "asset_size": 1234,
        });
        save_pending(
            &state,
            &Pending {
                tag: "v9.9.9-rc.1".into(),
                release: release.clone(),
                attempts: 0,
                next_try: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();
        let (claim, _) = queued_install(&state).await.unwrap();
        sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.channel','beta') ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .execute(&state.db)
            .await
            .unwrap();
        let _gate = state.update_lock.lock().await;
        still_wanted(&state, &Intent::Queued { claim: &claim })
            .await
            .expect("a pre-release queued on the beta channel is still wanted");

        // The channel changed while the package was being fetched. The decision
        // is taken where the queue is decided, so the install stops there.
        sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.channel','stable') ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .execute(&state.db)
            .await
            .unwrap();
        let error = still_wanted(&state, &Intent::Queued { claim: &claim })
            .await
            .unwrap_err();
        assert!(withdrawn(&error), "{error:#}");
    }

    #[tokio::test]
    async fn an_automatic_install_steps_aside_for_a_request_made_by_hand() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let release = json!({
            "tag": "v9.9.9", "version": "9.9.9", "prerelease": false, "supported": true,
            "asset_url": "https://github.com/Alexandre1116/diffrook/releases/download/v9.9.9/diffrook-linux-x86_64.tar.gz",
            "asset_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "asset_size": 1234,
        });
        let _gate = state.update_lock.lock().await;
        still_wanted(&state, &Intent::Automatic)
            .await
            .expect("with nothing queued, the policy may act");

        save_pending(
            &state,
            &Pending {
                tag: "v8.8.8".into(),
                release,
                attempts: 0,
                next_try: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();
        let error = still_wanted(&state, &Intent::Automatic).await.unwrap_err();
        assert!(
            withdrawn(&error) && format!("{error:#}").contains("v8.8.8"),
            "the automatic pass defers to the queued request and says which: {error:#}"
        );
        // A request for that same release is still allowed to proceed.
        still_wanted(&state, &Intent::Direct { tag: "v8.8.8" })
            .await
            .expect("the release the administrator asked for");
    }

    #[tokio::test]
    async fn lapsed_strikes_do_not_park_a_release_on_the_next_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let lapsed = (chrono::Utc::now() - chrono::Duration::hours(8)).to_rfc3339();
        let mut stored = std::collections::HashMap::new();
        stored.insert(
            "v9.9.9".to_string(),
            FailureCount {
                count: INSTALL_FAILURE_LIMIT,
                last_at: lapsed.clone(),
            },
        );
        sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.install_failures',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(serde_json::to_string(&stored).unwrap())
            .execute(&state.db)
            .await
            .unwrap();

        record_install_failure(&state, "v9.9.9", "the package is unusable", true).await;
        let counts = install_failures(&state).await;
        assert_eq!(
            counts["v9.9.9"].count, 1,
            "a fresh failure starts a fresh set, it does not resume an old one"
        );
        assert!(!counts["v9.9.9"].parked());
    }

    #[tokio::test]
    async fn a_transient_failure_does_not_keep_old_strikes_alive() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        let lapsed = (chrono::Utc::now() - chrono::Duration::hours(8)).to_rfc3339();
        let mut stored = std::collections::HashMap::new();
        stored.insert(
            "v9.9.9".to_string(),
            FailureCount {
                count: INSTALL_FAILURE_LIMIT,
                last_at: lapsed.clone(),
            },
        );
        sqlx::query("INSERT INTO app_settings(key,value) VALUES('updates.install_failures',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(serde_json::to_string(&stored).unwrap())
            .execute(&state.db)
            .await
            .unwrap();

        record_install_failure(&state, "v9.9.9", "GitHub timed out", false).await;
        let counts = install_failures(&state).await;
        assert_eq!(counts["v9.9.9"].last_at, lapsed, "the clock is not moved");
        assert!(
            !counts["v9.9.9"].parked(),
            "a night of network trouble must not revive a parked release"
        );
    }

    #[test]
    fn a_package_that_cannot_be_unpacked_is_not_chased_forever() {
        let dir = tempfile::tempdir().unwrap();
        let error =
            extract_release(dir.path(), "v9.9.9", b"not a gzip archive at all").unwrap_err();
        assert!(
            fatal(&error),
            "a broken package is the package's fault: {error:#}"
        );
    }

    #[tokio::test]
    async fn a_failed_install_tells_the_page_which_version_failed() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::core::AppState::new(dir.path()).await.unwrap();
        record_install_failure(&state, "v9.9.9", "the package is unusable", true).await;
        let error = install_error(&state).await;
        assert_eq!(error["tag"], "v9.9.9");
        assert_eq!(
            error["version"], "9.9.9",
            "the page adds its own v, so the tag must not be echoed as a version"
        );
        clear_install_failure(&state, "v9.9.9").await;
        assert!(install_error(&state).await.is_null());
    }

    #[test]
    fn automatic_updates_follow_the_release_the_page_offers() {
        let releases = vec![
            json!({"tag":"v2.0.0-beta.1","version":"2.0.0-beta.1","newer":true,"prerelease":true,"supported":true}),
            // A pre-release GitHub left unflagged: the stable channel ignores it.
            json!({"tag":"v1.9.0-beta.1","version":"1.9.0-beta.1","newer":true,"prerelease":false,"supported":true}),
            json!({"tag":"v1.8.0","version":"1.8.0","newer":true,"prerelease":false,"supported":true}),
            json!({"tag":"v1.7.0","version":"1.7.0","newer":true,"prerelease":false,"supported":true}),
        ];
        let none = std::collections::HashMap::new();
        assert_eq!(
            automatic_release(&releases, None, false, &none).as_deref(),
            Some("v1.8.0")
        );
        assert_eq!(
            automatic_release(&releases, None, true, &none).as_deref(),
            Some("v2.0.0-beta.1")
        );
        // The newest release on the channel is the only candidate: a rollback
        // marker stops the update instead of quietly installing an older release
        // the page never offered.
        assert_eq!(
            automatic_release(&releases, Some("v1.8.0"), false, &none),
            None
        );
        let mut failures = std::collections::HashMap::new();
        failures.insert(
            "v1.8.0".to_string(),
            FailureCount {
                count: INSTALL_FAILURE_LIMIT,
                last_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        assert_eq!(automatic_release(&releases, None, false, &failures), None);
        // The same holds when the offered release has no package for this host.
        let unsupported = vec![
            json!({"tag":"v1.9.0","version":"1.9.0","newer":true,"prerelease":false,"supported":false}),
            json!({"tag":"v1.8.0","version":"1.8.0","newer":true,"prerelease":false,"supported":true}),
        ];
        assert_eq!(
            automatic_release(&unsupported, None, false, &none),
            None,
            "no package for the offered release means no automatic install"
        );
        assert!(automatic_release(&[], None, false, &none).is_none());
    }

    #[test]
    fn beta_channel_includes_prereleases_but_not_missing_metadata() {
        let releases = vec![
            json!({"tag":"v2.0.0","version":"2.0.0","newer":true,"supported":true}),
            json!({"tag":"v1.9.0-beta.2","version":"1.9.0-beta.2","newer":true,"supported":false,"prerelease":true}),
            json!({"tag":"v1.9.0-beta.1","version":"1.9.0-beta.1","newer":true,"supported":true,"prerelease":true}),
            json!({"tag":"v1.8.0","version":"1.8.0","newer":true,"supported":true,"prerelease":false}),
        ];
        // The newest beta has no package for this platform, and neither does the
        // automatic installer invent one: it waits for the next release rather
        // than installing something the page never offered.
        assert_eq!(
            automatic_release(&releases, None, true, &std::collections::HashMap::new()),
            None
        );
        assert_eq!(
            automatic_release(
                &releases,
                Some("v1.9.0-beta.1"),
                true,
                &std::collections::HashMap::new()
            ),
            None
        );
        assert_eq!(
            automatic_release(&releases, None, false, &std::collections::HashMap::new()).as_deref(),
            Some("v1.8.0")
        );
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
            ("LICENSE", b"current license".as_slice()),
            ("licenses/previous.txt", b"prior license".as_slice()),
            ("web/licenses/LICENSE", b"compatible notice".as_slice()),
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
        assert_eq!(
            std::fs::read(extracted.join("LICENSE")).unwrap(),
            b"current license"
        );
        assert_eq!(
            std::fs::read(extracted.join("licenses/previous.txt")).unwrap(),
            b"prior license"
        );
        assert_eq!(
            std::fs::read(extracted.join("web/licenses/LICENSE")).unwrap(),
            b"compatible notice"
        );
        assert!(extract_release(dir.path(), "bad", b"not gzip").is_err());
    }

    #[test]
    fn rejects_unknown_paths_links_and_directories_in_place_of_required_files() {
        for (path, kind) in [
            ("secrets/key.pem", tar::EntryType::Regular),
            ("LICENSE/extra", tar::EntryType::Regular),
            ("licenses/link", tar::EntryType::Symlink),
            ("diffrook", tar::EntryType::Directory),
            ("web/index.html", tar::EntryType::Directory),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
            let archive = builder.into_inner().unwrap().finish().unwrap();
            assert!(
                extract_release(dir.path(), "v1.2.5", &archive).is_err(),
                "{path}"
            );
        }
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

    #[test]
    fn a_release_that_keeps_failing_to_install_is_left_alone() {
        let releases = vec![json!({
            "tag": "v1.5.0", "version": "1.5.0", "newer": true, "prerelease": false, "supported": true
        })];
        let mut failures = std::collections::HashMap::new();
        let recent = chrono::Utc::now().to_rfc3339();
        failures.insert(
            "v1.5.0".to_string(),
            FailureCount {
                count: INSTALL_FAILURE_LIMIT - 1,
                last_at: recent.clone(),
            },
        );
        assert_eq!(
            automatic_release(&releases, None, false, &failures).as_deref(),
            Some("v1.5.0"),
            "one more attempt is allowed before the release is left alone"
        );
        failures.insert(
            "v1.5.0".to_string(),
            FailureCount {
                count: INSTALL_FAILURE_LIMIT,
                last_at: recent,
            },
        );
        assert_eq!(
            automatic_release(&releases, None, false, &failures),
            None,
            "a package that keeps failing is not downloaded forever"
        );
        failures.insert(
            "v1.5.0".to_string(),
            FailureCount {
                count: INSTALL_FAILURE_LIMIT,
                last_at: (chrono::Utc::now() - chrono::Duration::hours(8)).to_rfc3339(),
            },
        );
        assert_eq!(
            automatic_release(&releases, None, false, &failures).as_deref(),
            Some("v1.5.0"),
            "the strikes expire, so a fixed package is tried again"
        );
    }

    #[test]
    fn a_release_offer_carries_its_own_failure_count() {
        let mut failures = std::collections::HashMap::new();
        failures.insert(
            "v2.0.0".to_string(),
            FailureCount {
                count: 2,
                last_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        let releases = vec![json!({
            "tag": "v2.0.0", "version": "2.0.0", "newer": true, "prerelease": false,
            "supported": true, "url": null, "published_at": null,
            "install_failures": release_failures(&failures, "v2.0.0"),
        })];
        let offer = offer_value(offered_release(&releases, false));
        assert_eq!(
            offer["install_failures"], 2,
            "Settings can say the last attempt failed"
        );
    }

    #[test]
    fn githubs_own_wait_outranks_the_backoff_ladder() {
        assert_eq!(next_wait(1, None), FIRST_RETRY);
        assert_eq!(next_wait(2, None), Duration::from_secs(120));
        assert_eq!(next_wait(3, None), Duration::from_secs(240));
        assert_eq!(
            next_wait(20, None),
            MAX_RETRY,
            "plain failures stop escalating"
        );
        assert_eq!(
            next_wait(1, Some(Duration::from_secs(7200))),
            Duration::from_secs(7200),
            "a reset GitHub reports is longer than any ladder value"
        );
        assert_eq!(next_wait(1, Some(Duration::from_secs(1))), FIRST_RETRY);
        assert_eq!(
            next_wait(1, Some(Duration::from_secs(30 * 24 * 3600))),
            Duration::from_secs(6 * 3600),
            "an absurd hint is still not slept through"
        );
    }
}
