use anyhow::{bail, Context, Result};

#[cfg(test)]
tokio::task_local! { static PRIVATE_MOCK: bool; }

#[cfg(test)]
pub(crate) async fn with_private_mock<T>(f: impl std::future::Future<Output = T>) -> T {
    PRIVATE_MOCK.scope(true, f).await
}

/// Only globally routable destinations are allowed by default. Private hosts
/// must be named by the operator, never by an API payload.
fn private_host_allowed(host: &str) -> bool {
    #[cfg(test)]
    if host == "127.0.0.1" && PRIVATE_MOCK.try_with(|allowed| *allowed).unwrap_or(false) {
        return true;
    }
    std::env::var("DIFFROOK_OUTBOUND_PRIVATE_HOSTS")
        .unwrap_or_default()
        .split(',')
        .any(|h| !h.trim().is_empty() && h.trim().eq_ignore_ascii_case(host))
}

fn public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            // Restrict IPv6 to global unicast and exclude special-purpose,
            // transition and documentation ranges. Mapped IPv4 is checked as IPv4.
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(v4.into());
            }
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn check_url(url: &url::Url) -> Result<()> {
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host().is_none()
    {
        bail!("Outbound URL must use HTTP(S) without credentials");
    }
    let host = url.host_str().unwrap();
    let allowed = private_host_allowed(host);
    match url.host().unwrap() {
        url::Host::Ipv4(ip) if !public_ip(ip.into()) && !allowed => {
            bail!("Outbound destination is not public")
        }
        url::Host::Ipv6(ip) if !public_ip(ip.into()) && !allowed => {
            bail!("Outbound destination is not public")
        }
        url::Host::Domain(h)
            if !allowed
                && (h.trim_end_matches('.').eq_ignore_ascii_case("localhost")
                    || h.ends_with(".localhost")
                    || h.eq_ignore_ascii_case("host.docker.internal")) =>
        {
            bail!("Outbound destination is not public")
        }
        _ => Ok(()),
    }
}

pub fn validate_url(raw: &str) -> Result<()> {
    check_url(&url::Url::parse(raw)?)
}

fn check_addresses(addresses: &[std::net::SocketAddr], allow_private: bool) -> Result<()> {
    if addresses.is_empty() || (!allow_private && addresses.iter().any(|a| !public_ip(a.ip()))) {
        bail!("Outbound DNS destination is not public");
    }
    Ok(())
}

pub trait SafeSend {
    fn send_safe(self) -> impl std::future::Future<Output = Result<reqwest::Response>> + Send;
}

impl SafeSend for reqwest::RequestBuilder {
    async fn send_safe(self) -> Result<reqwest::Response> {
        let request = self.build()?;
        let url = request.url();
        check_url(url)?;
        let host = url.host_str().unwrap();
        let port = url.port_or_known_default().unwrap();
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30));
        if matches!(url.host(), Some(url::Host::Domain(_))) {
            let addresses: Vec<_> = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tokio::net::lookup_host((host, port)),
            )
            .await
            .context("Outbound DNS lookup timed out")??
            .collect();
            check_addresses(&addresses, private_host_allowed(host))?;
            // Pin the validated result; the HTTP connection must not resolve again.
            builder = builder.resolve_to_addrs(host, &addresses);
        }
        builder
            .build()?
            .execute(request)
            .await
            .context("Outbound request failed")
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_rejects_empty_and_mixed_public_private_answers() {
        let public = "1.1.1.1:443".parse().unwrap();
        let private = "10.0.0.1:443".parse().unwrap();
        assert!(check_addresses(&[], false).is_err());
        assert!(check_addresses(&[public, private], false).is_err());
        assert!(check_addresses(&[public], false).is_ok());
        assert!(check_addresses(&[private], true).is_ok());
    }

    #[tokio::test]
    async fn automatic_redirects_are_disabled_even_for_a_permissive_input_client() {
        use crate::scm::test_server::{serve, Reply};
        with_private_mock(async {
            let (origin, server) = serve(vec![Reply {
                status: "302 Found",
                headers: vec![("Location", "http://169.254.169.254/latest/meta-data".into())],
                body: String::new(),
            }])
            .await;
            let response = reqwest::Client::new()
                .get(origin)
                .send_safe()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::FOUND);
            assert_eq!(server.await.unwrap().len(), 1);
        })
        .await;
    }

    #[test]
    fn only_public_addresses_are_accepted() {
        for raw in [
            "0.0.0.0",
            "10.1.2.3",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "198.18.0.1",
            "224.1.1.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "2002:7f00:1::",
            "64:ff9b::7f00:1",
            "2001:db8::1",
        ] {
            assert!(!public_ip(raw.parse().unwrap()), "{raw}");
        }
        for raw in ["8.8.8.8", "1.1.1.1", "192.0.64.1", "2606:4700:4700::1111"] {
            assert!(public_ip(raw.parse().unwrap()), "{raw}");
        }
    }

    #[tokio::test]
    async fn requests_to_internal_destinations_fail_before_connecting() {
        let client = reqwest::Client::new();
        for url in [
            "http://127.0.0.1:9",
            "http://2130706433:9",
            "http://0x7f000001:9",
            "http://[::ffff:127.0.0.1]:9",
            "http://localhost:9",
            "http://host.docker.internal:9",
            "http://169.254.169.254/latest/meta-data",
            "file:///etc/passwd",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
            assert!(client.get(url).send_safe().await.is_err(), "{url}");
        }
        assert!(validate_url("https://user:password@example.com").is_err());
    }
}
