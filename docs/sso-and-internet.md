# SSO and Internet deployments

Each company runs an independent Diffrook installation with its own URL, identity
provider, credentials and data volume. No central Diffrook domain is required.
`DIFFROOK_PUBLIC_URL` is the canonical URL used by employees to open that
installation. It can also be a private HTTPS hostname.

## Configure OpenID Connect

Register a confidential web application with the company's OpenID Connect
provider. Use the authorization code flow and PKCE S256. Diffrook requests the
`openid` and `email` scopes. Register exactly this callback, substituting the
installation's URL:

```text
https://reviews.your-company.example/api/auth/oidc/callback
```

Configure the installation, for example through `.env` and Compose:

```dotenv
DIFFROOK_PUBLIC_URL=https://reviews.your-company.example
DIFFROOK_SECURE_COOKIES=true
DIFFROOK_OIDC_ISSUER_URL=https://identity.your-company.example/your-issuer
DIFFROOK_OIDC_CLIENT_ID=your-application-client-id
DIFFROOK_OIDC_CLIENT_SECRET_FILE=/run/secrets/oidc_client_secret
DIFFROOK_OIDC_ALLOWED_SUBJECTS=exact-subject-claim-of-authorized-administrator
DIFFROOK_LOCAL_LOGIN=false
```

Use the exact issuer from the provider's discovery document. Diffrook checks that
the discovery issuer matches the configured value. The issuer, discovery, JWKS,
authorization and token endpoints must use HTTPS. Redirects are disabled for
backchannel requests. The provider must advertise a signing algorithm and public
keys supported by the `openidconnect` library.

This is generic OIDC support intended for providers such as Microsoft Entra ID,
Authentik and Keycloak. Their application registration, issuer and claim settings
differ. This implementation has been tested with a mock OIDC provider; a real
provider integration must be exercised in each company's staging installation.

`DIFFROOK_OIDC_ALLOWED_SUBJECTS` contains exact, comma-separated `sub` claims for
this application. A provider's internal user ID or Entra `oid` claim is not
necessarily the same as `sub`. Prefer subjects because emails can change.
Alternatively, `DIFFROOK_OIDC_ALLOWED_EMAILS` accepts exact email addresses only
when the signed ID token includes `email_verified: true`. An email domain alone
never grants access. If the provider does not emit a verified email claim, use
subjects. At least one explicit allowlist is required.

Every allowed SSO identity receives full administrator access to this single
installation, including repository credentials and automation write settings.
Diffrook does not currently have viewer/member roles. Only allow administrators.
Assign the IdP application to the appropriate administrators and require MFA in
the IdP's policy.

SSO defaults to disabling local authentication, including initial password setup.
New SSO identities require an active Freelancer, Teams or Enterprise license and
consume its user allowance, including any existing local administrator. Individual
uses local sign-in. Previously provisioned SSO identities retain verified sign-in
access after expiry or upgrade; legacy signed Business licenses remain supported.
See [plans and licenses](licensing.md) for SSO-only bootstrap and one-user migration.
An allowlisted administrator can sign in directly on a fresh licensed installation.
Existing local sessions stop working when local login is disabled. Explicitly
set `DIFFROOK_LOCAL_LOGIN=true` only if a separate local administrator login is
needed. SSO identities are never linked to local accounts by email or username.

The client secret remains on the server. Mount a Docker secret using an overlay:

```yaml
services:
  diffrook:
    secrets:
      - oidc_client_secret
secrets:
  oidc_client_secret:
    file: ./secrets/oidc-client-secret.txt
```

The secret file must be readable by the application container's UID 10001. Keep
the host file and `.env` private, outside version control. A plain
`DIFFROOK_OIDC_CLIENT_SECRET` environment variable is also supported, but do not
configure both secret sources. SSO settings are deployment configuration and are
not editable or exported through the web UI.

## Sessions and revocation

Authorization uses PKCE, a nonce and a one-time state bound to an HttpOnly browser
cookie. Pending logins expire after five minutes. ID tokens are checked for
signature, issuer, audience, expiry, nonce, issue time and any access-token hash.
State is consumed atomically before token exchange to reject replay. Pending
nonces and PKCE verifiers are encrypted in SQLite. Access/refresh/ID tokens are
not retained or sent to the browser.

Application sessions default to eight hours. `DIFFROOK_SESSION_SECONDS` accepts
300 through 86400 seconds. HTTPS cookies use `Secure`, `HttpOnly`, host-only
`__Host-` names and `SameSite=Strict`; the temporary OIDC correlation cookie uses
`SameSite=Lax` so the IdP's top-level GET callback works. Use query-mode callbacks,
not `form_post`. Logging out revokes the application session; it does not log the
user out of the company's IdP.

The configured allowlist is checked on each authenticated request. Removing an
identity and restarting the application invalidates its access immediately.
Disabling an account only in the IdP does not immediately revoke an existing
Diffrook session. That session lasts until local logout or expiry. Choose a
shorter session lifetime if this matters to the company's policy. Diffrook does
not implement IdP backchannel logout, SCIM or continuous token introspection.

## Expose an installation through HTTPS

Use the company's existing HTTPS reverse proxy or the optional Caddy overlay:

```sh
docker compose -f compose.yaml -f compose.public.yaml up --build -d
```

Set `DIFFROOK_DOMAIN` to that company's installation hostname. For public Caddy
certificate issuance, its DNS must point to the server and ports 80 and 443 must
be reachable. Keep `DIFFROOK_BIND_ADDRESS=127.0.0.1`; the backend's port 8080 stays
bound to the host loopback interface. Only the proxy's ports are exposed publicly.
The overlay overrides the public URL and secure-cookie settings.

The example uses proxy IP `172.30.99.2` on subnet `172.30.99.0/24`. Change the subnet,
addresses and `DIFFROOK_TRUSTED_PROXIES` together if this conflicts with an existing
network. Pin the chosen container images to reviewed versions/digests in the
company's deployment process.

For another proxy, preserve the public `Host`, including any non-default port,
and overwrite `X-Real-IP` with the actual client IP. Configure only the proxy's
exact source IP in `DIFFROOK_TRUSTED_PROXIES`. Do not trust forwarded IP headers
from arbitrary clients. Do not expose 8080 directly or let clients bypass the
proxy. Webhooks use the same HTTPS URL and their own HMAC signatures; SSO must
not intercept `/api/webhooks/*`.

Diffrook validates the configured host and origin, requires its custom CSRF header
on API mutations including login/setup/logout, sends CSP and other browser
security headers, and marks API/HTML responses `no-store`. Its in-process limiter
allows 30 authentication requests and 1200 other requests per client IP per minute,
with a global 6000-request cap and bounded tracking state. Password verification
runs off the async executor with at most two concurrent jobs. Also set connection,
request-rate and timeout limits at the proxy; the application limiter is not DDoS
protection and resets on restart.

The Caddy example also limits headers to 32 KB and bodies to 2 MB, with 10-second
header, 30-second body, 60-second response and two-minute idle timeouts. Adjust
these to the installation's measured workload. It preserves the original Host
and overwrites the client IP header before forwarding.

The persistent run queue is capped at 1000 waiting jobs. Webhooks that cannot be
fully queued return an error and can be redelivered without duplicating jobs
already saved by that delivery. Run history and delivery receipts currently have
no automatic retention policy; monitor the data volume and plan history retention
before operating at high event volumes. Confirmed in-container updates can be
replaced by later updates, and update staging prevents the worker from claiming
new jobs before restart.

Before enabling real repositories, complete a login/logout with the company's
IdP, verify an unauthorized account is rejected, test webhook delivery, inspect
TLS/host/header behavior at the actual proxy and test a backup restoration. Keep
automatic updates disabled until updates have been exercised in staging. AI
providers receive selected repository content; choose an endpoint and data policy
appropriate to the company's private code.

## Verification and scope

The frontend dependency audit is clean after updating `source-map-js`. The Rust
audit reports `RUSTSEC-2023-0071` in the OIDC library's `rsa` dependency. That
[advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) concerns private-key
timing leakage and currently has no patched release. Production Diffrook only
performs public-key signature verification and holds no RSA private key. CI has
an explicit exception for that one advisory, with this applicability assessment;
it continues to fail on other dependency vulnerabilities. Remove/reassess the
exception if signing, decryption or private RSA keys are ever introduced. An
unfiltered audit will still report the advisory.

The automated suite covers valid and invalid signed OIDC tokens, PKCE transmission,
browser binding, expiry and concurrent replay; authorization and account identity;
SSO-only mode and legacy-session migration; CSRF, secure cookies, host validation,
spoofed proxy headers, rate limiting, request size and database health failures.
The CI also runs the repository/provider integration smoke test with mock services.

Local validation on 2026-10-09 passed 41 Rust tests, Clippy with warnings denied,
the frontend production build, and all 14 integration smoke checks against the
Linux binary and mock services. Both Compose configurations passed syntax
validation, and Caddy 2.10.2 accepted the proxy configuration. A separate loopback
TLS test using Caddy's local CA verified the certificate, host with a non-default
port, CSRF checks, cookie flags, logout revocation and overwritten client IPs.
The local CA was supplied only to the test client. It was not installed into the
machine's trust store. The Docker daemon was unavailable here, so the actual
container build/run remains a CI/deployment check. Public certificate issuance
and a real company's IdP were not exercised.

These checks are an application security review, not a penetration-test certificate
or an availability guarantee. TLS termination, IdP MFA/assignment policies, firewall
rules, backups, credential scope and monitoring belong to each installation.
