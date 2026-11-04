# Security and release verification

## Audit scope

The November 2026 security audit assessed Diffrook v0.2.0-beta.3 in a self-hosted
deployment. Diffrook Cloud and its hosted infrastructure were outside the audit
scope. Version 1.0.0 contains the remediation and verification described below.

## November 2026 review

The v1.0.0 source addresses the two confirmed application findings in the
November 1 pentest: outbound SSRF and persistence of undocumented API fields.
The original beta.3 packages do not contain these fixes. Upgrade to v1.0.0;
back up the complete data volume first.

## Outbound requests

Code-host connections, AI providers and notification webhooks only contact
public HTTP(S) destinations by default. Requests reject private, loopback,
link-local, metadata, multicast and reserved IPs, including mapped IPv4.
Every request validates all DNS answers and pins them for the connection;
automatic redirects and environment proxies are disabled. Saved integrations
also pass these checks when used, including runs and connection tests.

For private Git servers or Ollama, configure exact comma-separated hosts in
`DIFFROOK_OUTBOUND_PRIVATE_HOSTS`, for example `host.docker.internal`.
The default is empty. An exception grants that host access to private addresses
on any port; do not allow untrusted hostnames. Wildcards and CIDRs are unsupported.
Restart after changing deployment configuration. This policy applies to API
integrations, not the operator-configured OIDC issuer or the fixed update service.

## API inputs

Object creation and updates use dedicated typed schemas with
`serde(deny_unknown_fields)`. Unknown fields, including nested automation settings
and notification destinations, return HTTP 400 before persistence. Server-owned
response metadata is discarded on editor round trips. IDs and timestamps come
from the server. Existing credentials are retained when an update omits them.
Existing historical unknown fields are not migrated automatically; editing an
object writes only its supported fields.

## OIDC test signing key

`tests/fixtures/oidc-test-key.pem` is a disposable Ed25519 fixture with key ID
`test-key`. Its only inclusion is inside `#[cfg(test)]` in `src/sso.rs`, including
at pentest commit `535ce07e10a297fbf148ef73835454a20a63cff9`.

On November 4, both official beta.3 archives were downloaded from GitHub and
their SHA-256 checksums verified. The x86-64 and ARM64 production binaries contain
one `BEGIN PRIVATE KEY` delimiter, but none of this fixture's PEM, base64, DER or
32-byte private seed. A delimiter count alone does not establish key exposure.
The reported F-03 is not reproduced in these official artifacts. This does not
identify the bytes in an independently built or deployed image from the pentest.

Archive SHA-256 values:

```text
x86_64: 822e4b287fa25b7b4ad3ae8c4545448107de10c5f8921b4ed3b5033092db5ea1
aarch64: 672bcae236fa3f65df6a1d193f7361d34ecaddb4804bc72ef70d9ec8784914b6
```

CI checks the release binary and each packaged architecture for actual fixture
material using `python3 scripts/check-release-test-key.py BINARY_OR_TAR_GZ`.
The fixture must never be used by a real identity provider.

## Dependencies and container

Vite is updated to 8.3.5 and Tokio to 1.54.1. The runtime uses Distroless Debian 13
with UID/GID 10001, a read-only filesystem, no Linux capabilities, no shell and no
package manager. `/data` remains writable. Docker health checks use the binary's
`--healthcheck` command. Release update archives keep the glibc-compatible Rust
binary, static web assets and licence notices.

Two narrowly scoped RustSec exceptions remain in CI:

- [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071): `rsa`
  private-key timing leakage; there is no patched stable release.
- [RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387):
  `crypto-bigint` secret-integer timing variability. The OIDC dependency chain
  still requires the affected 0.5 series.

Production OIDC only verifies public signatures; it does not sign or decrypt
using RSA/EC private keys. This is an applicability assessment, not removal of
the affected dependencies. Reassess if private-key operations are introduced.
Unfiltered `cargo audit` still reports these advisories; new advisories are not
covered by the exceptions.

## Validation limits

Local v1.0.0 verification on November 4 passed 80 Rust tests, Clippy with warnings
denied, the frontend build, npm audit with zero vulnerabilities, 18 container
integration checks, and the release-binary key scan. Trivy reported zero HIGH or
CRITICAL findings in the pinned Distroless runtime image. The same-day rebuilt
Debian slim baseline reported 44 HIGH/CRITICAL findings before the migration.
These scanner results apply to their database snapshot, not future advisories.

Regression tests cover strict create/update inputs, blocked destinations,
mixed public/private DNS answers, redirects, encrypted credentials, authentication,
SSO, quotas and ended beta entitlements. Container smoke tests exercise code-host
reviews, fixes, AI providers, notifications, cancellation and backup restoration
with isolated mock services. These checks do not replace a production retest.

Prompt-injection testing with a real repository and production model remains
outside this review. TLS termination, IdP policy, network egress controls and
backup operations remain installation responsibilities.
