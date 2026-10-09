use crate::core::{error, AppState};
use anyhow::{ensure, Context};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::Response,
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub allowed_subjects: Vec<String>,
    pub allowed_emails: Vec<String>,
    #[cfg(test)]
    pub allow_test_http: bool,
}

impl OidcConfig {
    pub fn allows(&self, subject: &str, email: Option<&str>, verified: bool) -> bool {
        self.allowed_subjects.iter().any(|s| s == subject)
            || (verified
                && email.is_some_and(|email| {
                    self.allowed_emails
                        .iter()
                        .any(|e| e.eq_ignore_ascii_case(email))
                }))
    }

    pub fn validate_endpoint(&self, value: &str) -> anyhow::Result<()> {
        let url = Url::parse(value)?;
        let secure = url.scheme() == "https";
        #[cfg(test)]
        let secure = secure
            || (self.allow_test_http
                && url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1"));
        ensure!(
            secure
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "OIDC endpoints must use HTTPS without embedded credentials or fragments"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct SecurityConfig {
    pub public_url: Option<Url>,
    pub secure_cookies: bool,
    pub local_login: bool,
    pub session_seconds: i64,
    pub trusted_proxies: Vec<IpAddr>,
    pub oidc: Option<OidcConfig>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            public_url: None,
            secure_cookies: false,
            local_login: true,
            session_seconds: 8 * 3600,
            trusted_proxies: vec![],
            oidc: None,
        }
    }
}

impl SecurityConfig {
    pub fn session_cookie_name(&self) -> &'static str {
        if self.secure_cookies {
            "__Host-diffrook_session"
        } else {
            "diffrook_session"
        }
    }
    pub fn oidc_cookie_name(&self) -> &'static str {
        if self.secure_cookies {
            "__Host-diffrook_oidc"
        } else {
            "diffrook_oidc"
        }
    }
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let public_url = get("DIFFROOK_PUBLIC_URL")
            .map(|v| Url::parse(&v))
            .transpose()?;
        if let Some(url) = &public_url {
            let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            ensure!(
                url.scheme() == "https" || (url.scheme() == "http" && loopback),
                "DIFFROOK_PUBLIC_URL must use HTTPS, except on localhost"
            );
            ensure!(url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
                && url.path() == "/" && url.query().is_none() && url.fragment().is_none(), "DIFFROOK_PUBLIC_URL must be an origin, without a path, credentials, query or fragment");
        }
        let https = public_url.as_ref().is_some_and(|u| u.scheme() == "https");
        let secure_cookies = boolean(get("DIFFROOK_SECURE_COOKIES"), https)?;
        ensure!(
            !https || secure_cookies,
            "HTTPS public URLs require secure cookies"
        );
        let issuer = get("DIFFROOK_OIDC_ISSUER_URL");
        let client_id = get("DIFFROOK_OIDC_CLIENT_ID");
        let secret_file = get("DIFFROOK_OIDC_CLIENT_SECRET_FILE");
        let secret_env = get("DIFFROOK_OIDC_CLIENT_SECRET");
        ensure!(
            secret_file.is_none() || secret_env.is_none(),
            "Configure only one OIDC client secret source"
        );
        let subjects = list(get("DIFFROOK_OIDC_ALLOWED_SUBJECTS"));
        let emails = list(get("DIFFROOK_OIDC_ALLOWED_EMAILS"));
        let any_oidc = issuer.is_some()
            || client_id.is_some()
            || secret_file.is_some()
            || secret_env.is_some()
            || !subjects.is_empty()
            || !emails.is_empty();
        let oidc = if any_oidc {
            ensure!(
                https && secure_cookies,
                "SSO requires an HTTPS DIFFROOK_PUBLIC_URL and secure cookies"
            );
            let issuer = issuer.context("DIFFROOK_OIDC_ISSUER_URL is required")?;
            let client_id = client_id.context("DIFFROOK_OIDC_CLIENT_ID is required")?;
            let client_secret = if let Some(path) = secret_file {
                ensure!(
                    std::fs::metadata(&path)?.len() <= 65536,
                    "OIDC client secret file is too large"
                );
                std::fs::read_to_string(path)?.trim().to_owned()
            } else {
                secret_env.context("An OIDC client secret or secret file is required")?
            };
            ensure!(
                !client_secret.is_empty()
                    && client_secret.len() <= 65536
                    && client_id.len() <= 1024,
                "Invalid OIDC client credentials"
            );
            ensure!(
                !subjects.is_empty() || !emails.is_empty(),
                "SSO requires an explicit allowed subject or verified email list"
            );
            let config = OidcConfig {
                issuer,
                client_id,
                client_secret,
                allowed_subjects: subjects,
                allowed_emails: emails,
                #[cfg(test)]
                allow_test_http: false,
            };
            config.validate_endpoint(&config.issuer)?;
            ensure!(
                Url::parse(&config.issuer)?.query().is_none(),
                "OIDC issuer cannot contain a query"
            );
            Some(config)
        } else {
            None
        };
        let local_login = boolean(get("DIFFROOK_LOCAL_LOGIN"), oidc.is_none())?;
        ensure!(
            local_login || oidc.is_some(),
            "Disabling local login requires configured SSO"
        );
        let session_seconds = get("DIFFROOK_SESSION_SECONDS")
            .map(|v| v.parse::<i64>())
            .transpose()?
            .unwrap_or(8 * 3600);
        ensure!(
            (300..=86400).contains(&session_seconds),
            "Session lifetime must be between 300 and 86400 seconds"
        );
        let trusted_proxies = list(get("DIFFROOK_TRUSTED_PROXIES"))
            .iter()
            .map(|s| s.parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            public_url,
            secure_cookies,
            local_login,
            session_seconds,
            trusted_proxies,
            oidc,
        })
    }

    pub fn origin_ok(&self, headers: &HeaderMap) -> bool {
        if !crate::core::request_origin_ok(headers) {
            return false;
        }
        if headers
            .get("sec-fetch-site")
            .is_some_and(|s| s == "cross-site")
        {
            return false;
        }
        match (&self.public_url, headers.get(header::ORIGIN)) {
            (Some(public), Some(origin)) => origin
                .to_str()
                .ok()
                .is_some_and(|o| o == public.origin().ascii_serialization()),
            _ => true,
        }
    }

    pub fn cookie(&self, name: &str, value: &str, seconds: i64, lax: bool) -> HeaderValue {
        HeaderValue::from_str(&format!(
            "{name}={value}; HttpOnly; SameSite={}; Path=/; Max-Age={seconds}{}",
            if lax { "Lax" } else { "Strict" },
            if self.secure_cookies { "; Secure" } else { "" }
        ))
        .expect("generated cookie is ASCII")
    }
}

fn boolean(value: Option<String>, default: bool) -> anyhow::Result<bool> {
    match value.as_deref() {
        None => Ok(default),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        _ => anyhow::bail!("Boolean settings must be true or false"),
    }
}
fn list(value: Option<String>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub struct RateLimiter {
    clients: HashMap<(IpAddr, bool), (Instant, u32)>,
    global: (Instant, u32),
}
impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            clients: HashMap::new(),
            global: (Instant::now(), 0),
        }
    }
}
impl RateLimiter {
    pub fn allow(&mut self, ip: IpAddr, auth: bool) -> bool {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        if now.duration_since(self.global.0) >= window {
            self.global = (now, 0);
            self.clients
                .retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        self.global.1 = self.global.1.saturating_add(1);
        if self.global.1 > 6000 {
            return false;
        }
        let key = (ip, auth);
        if !self.clients.contains_key(&key) && self.clients.len() >= 4096 {
            return false;
        }
        let bucket = self.clients.entry(key).or_insert((now, 0));
        if now.duration_since(bucket.0) >= window {
            *bucket = (now, 0);
        }
        bucket.1 = bucket.1.saturating_add(1);
        bucket.1 <= if auth { 30 } else { 1200 }
    }
}

pub async fn protect(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let config = &state.security;
    let path = request.uri().path();
    let mut rejection = None;
    if request.uri().to_string().len() > 8192 {
        rejection = Some(error(StatusCode::URI_TOO_LONG, "Request URL is too long"));
    }
    if path != "/healthz" {
        if let Some(public) = &config.public_url {
            let expected = &public[url::Position::BeforeHost..url::Position::AfterPort];
            if request
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                != Some(expected)
            {
                rejection = Some(error(
                    StatusCode::MISDIRECTED_REQUEST,
                    "Unexpected request host",
                ));
            }
        }
    }
    if path.starts_with("/api/")
        && !path.starts_with("/api/webhooks/")
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
        && !config.origin_ok(request.headers())
    {
        rejection = Some(error(
            StatusCode::FORBIDDEN,
            "Invalid request origin or CSRF header",
        ));
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|p| p.0.ip())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]));
    let ip = if config.trusted_proxies.contains(&peer) {
        request
            .headers()
            .get("x-real-ip")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(peer)
    } else {
        peer
    };
    let auth = matches!(path, "/api/login" | "/api/setup") || path.starts_with("/api/auth/");
    if path != "/healthz" && !state.rate_limiter.lock().await.allow(ip, auth) {
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Try again in one minute",
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        rejection = Some(response);
    }
    let private = path.starts_with("/api/") || path == "/" || !path.contains('.');
    let mut response = match rejection {
        Some(response) => response,
        None => next.run(request).await,
    };
    let headers = response.headers_mut();
    for (name, value) in [
        ("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
        ("x-content-type-options", "nosniff"), ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"), ("permissions-policy", "camera=(), microphone=(), geolocation=()"),
    ] { headers.insert(name, HeaderValue::from_static(value)); }
    if private
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"))
    {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if config.secure_cookies {
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response
}

// API routes are also protected when mounted directly in tests or another host.
// The binary adds this wrapper to cover the static frontend without counting API requests twice.
pub async fn protect_static(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if request.uri().path().starts_with("/api/") || request.uri().path() == "/healthz" {
        next.run(request).await
    } else {
        protect(State(state), request, next).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deployment_configuration_fails_closed() {
        let parse = |pairs: &[(&str, &str)]| {
            SecurityConfig::from_lookup(|key| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert!(parse(&[("DIFFROOK_PUBLIC_URL", "http://example.com")]).is_err());
        assert!(parse(&[
            ("DIFFROOK_PUBLIC_URL", "https://example.com"),
            ("DIFFROOK_SECURE_COOKIES", "false")
        ])
        .is_err());
        assert!(parse(&[("DIFFROOK_LOCAL_LOGIN", "false")]).is_err());
        assert!(parse(&[("DIFFROOK_OIDC_ISSUER_URL", "https://id.example.com")]).is_err());
        assert!(parse(&[("DIFFROOK_PUBLIC_URL", "https://example.com/path")]).is_err());
        assert!(
            parse(&[("DIFFROOK_PUBLIC_URL", "https://example.com")])
                .unwrap()
                .secure_cookies
        );
    }
    #[test]
    fn rate_limit_is_bounded_and_separates_authentication() {
        let mut limiter = RateLimiter::default();
        let ip = "192.0.2.1".parse().unwrap();
        for _ in 0..30 {
            assert!(limiter.allow(ip, true));
        }
        assert!(!limiter.allow(ip, true));
        assert!(limiter.allow(ip, false));
        assert!(limiter.allow("192.0.2.2".parse().unwrap(), true));
    }
}
