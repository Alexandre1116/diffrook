# Plans and self-hosted licenses

The Individual plan is free for one person's personal, noncommercial use. It
permits one registered Diffrook account and three saved automations per
installation. Disabled automations count. Company, team, employment, freelance,
consulting and other professional work requires a paid Business license, even
when there is only one user. Repository bot credentials are connections, not
Diffrook login accounts. Infrastructure and model-provider costs are separate.

The project owner may offer paid hosted subscriptions in the future. Hosted
service terms will be separate from self-hosted Business licenses. No hosted
service, billing integration or SLA is included by this change. Third parties
need separate written authorization to offer Diffrook hosting or resale.

Read [LICENSE](../LICENSE) for the terms. This is a project-specific license
draft and should receive legal review before commercial sales. It preserves
rights already granted for material published under CC BY-NC 4.0, whose original
text is retained in [docs/licenses](licenses/CC-BY-NC-4.0.txt). It does not
retroactively impose new restrictions on those grants. Third-party licenses
remain applicable.

Creative Commons explains that [previous CC grants are irrevocable](https://creativecommons.org/faq/#what-if-i-change-my-mind-about-using-a-cc-license).

## Install a Business license

1. Start Diffrook and open **Settings** as an administrator. Copy its
   **Installation ID**. The ID is persisted in the data volume's SQLite settings.
2. Obtain a paid license for that ID from the project owner under a written
   commercial agreement. The signed file specifies the customer, user and
   automation allowances, and optional expiry. It is not a purchase receipt or
   a substitute for the agreement.
3. Mount the file read-only and set `DIFFROOK_LICENSE_FILE` to its container path.
   Restart Diffrook, then check Settings for the active Business plan.

Example overlay:

```yaml
services:
  diffrook:
    environment:
      DIFFROOK_LICENSE_FILE: /run/secrets/diffrook_license
    secrets:
      - diffrook_license
secrets:
  diffrook_license:
    file: ./secrets/customer-license.json
```

The file must be readable by UID 10001. It is limited to 32 KB and verified with
the Ed25519 public key compiled into the application. Invalid signatures, wrong
installation IDs and malformed files fail startup. A valid but expired license
is shown as expired and uses Individual creation limits. Expiry is checked when
an operation occurs, not only at startup. A missing license also uses Individual.
There is no `DIFFROOK_PLAN=business` switch or configurable Individual quota.
No licensing server is contacted and no license telemetry is sent.

New SSO identities count as registered users. An existing identity can sign in
again without consuming another slot. Local and SSO identities are separate and
are not automatically linked; for an Individual installation, choose one login
identity. The existing local setup endpoint creates only the initial administrator,
including on Business. Additional Business users currently enter through SSO.
All users have administrator access, so license allowances are not authorization
roles. Only allow the intended administrators in the IdP and OIDC allowlist.

The server enforces automation creations using a single SQLite write, including
parallel API requests. Sending a plan or quota in JSON cannot override it. Editing
or deleting an existing automation remains possible at the limit. An edit cannot
recreate a concurrently deleted automation to bypass the quota.

Existing installations above a quota, or an expired Business license, retain
their records and login access. Settings displays the usage and over-limit state.
New creations are blocked for each resource that has reached its allowance.
Nothing is automatically deleted or disabled, including existing automation
execution. Reduce usage or renew the license. Retained technical access does not
extend a contractual right to continue operating for business purposes.

## Issue licenses as the project owner

The issuer key was initialized for this implementation:

- `src/license-public-key.hex` is public and is included in official builds.
- `secrets/license-signing-key.pem` is private and excluded from Git and Docker
  build context. Back it up securely. Never distribute it with a customer instance.

Only the owner with this private key can issue licenses recognized by official
builds. During initial issuer setup, with both key files absent, `node
scripts/license.mjs init` generates a pair and refuses to overwrite existing files. Commit the
public key before building. Do not rerun initialization to replace a key: losing
or rotating it requires a deliberate migration of official builds and licenses.

For a perpetual self-hosted license with no numerical allowances:

```sh
node scripts/license.mjs issue --installation CUSTOMER_UUID --customer "Company name" --out ./secrets/customer-license.json
```

For a term license with specific allowances:

```sh
node scripts/license.mjs issue --installation CUSTOMER_UUID --customer "Company name" --expires 2027-10-09T00:00:00Z --users 20 --automations 100 --out ./secrets/customer-license.json
```

Omitting a numerical allowance makes that resource unlimited; omitting expiry
makes the technical entitlement perpetual. Choose these deliberately to match
the agreement. Output files are exclusively created; use a new filename for a
renewal, mount it and restart. Keep a record of issued license IDs and contracts.
Backups retain the same Installation ID and entitlement. Do not operate cloned
volumes as extra licensed installations unless the agreement permits it.

Because customers control their servers and can inspect source, technical checks
cannot prevent all modifications, database edits or cloned installations. The
signed file prevents inventing or editing entitlements in official builds; the
license agreement defines permitted use. Self-hosted installations have no
online revocation check. A lost key must be handled as an issuer security event.

## Validation

This change passed 51 Rust tests, the Node issuer CLI test, Clippy with warnings
treated as errors, the frontend production build and 15 integration smoke checks.
The quota checks cover concurrent automation creations, concurrent SSO user
provisioning, disabled automations, edits at the limit and retained legacy data.
A locally issued, instance-bound Business license was also tested against the
running server: its allowances applied, removing it restored Individual limits
without deleting records, and another installation rejected it. Docker Compose
configuration was validated; a Docker image build was not run locally because
the Docker daemon was unavailable.
