# Plans and self-hosted licenses

Diffrook currently runs on the customer's server. Individual is free forever
for personal, noncommercial use, without a trial or expiry. Professional,
freelance, employment, client, company and team use requires a paid plan,
even for one user. Prices below are in EUR before applicable tax.

| Self-hosted plan | Registered users | Saved automations | SSO | Monthly | Annual |
| --- | ---: | ---: | :---: | ---: | ---: |
| Individual | 1 | 3 | No | Free forever | Free |
| Freelancer | 1 | 10 | No | 9 | 90 |
| Teams | 5 | 20 | Yes | 29 | 290 |
| Enterprise | Purchased seats, minimum 10 | 10 per purchased seat, pooled | Yes | 8/seat | 80/seat |

Enterprise starts at EUR 80/month or EUR 800/year for ten users and 100 automations.
Enterprise-only packs add ten automations for EUR 5/month or EUR 50/year. Annual
subscriptions cost ten monthly payments, paid up front. Paid plans include
software updates during their term; response times and availability SLAs require
a separate agreement. Disabled automations count. Repository bot credentials
are connections, not Diffrook users.

All self-hosted plans provide their own server and AI provider credentials or
local model. There is no execution-based license charge; infrastructure and
AI bills are paid directly by the customer. The embedded [catalog](../src/plans.json)
is shared by the server, owner issuer and Settings. `GET /api/plans` returns only
public prices. Authenticated `GET /api/license` returns installation metadata
and usage. Sending prices or quotas in a request cannot activate a plan.

## Cloud Hosting: Coming soon

These are preview prices for the future owner-hosted service. Cloud cannot be
purchased or activated; the issuer cannot create Cloud licenses. Managed AI
metering, hosted operations and billing are not available. All hosted plans are paid.

| Planned Cloud plan | Users | Saved automations | SSO | Monthly | Annual | Executions/month |
| --- | ---: | ---: | :---: | ---: | ---: | ---: |
| Individual | 1 | 3 | No | 9 | 90 | 500 |
| Freelancer | 1 | 10 | No | 19 | 190 | 2,000 |
| Teams | 5 | 20 | Yes | 59 | 590 | 10,000 |
| Enterprise | Minimum 10 | 10 per seat, pooled | Yes | 15/seat | 150/seat | 2,000/seat |

Cloud Enterprise starts at EUR 150/month or EUR 1,500/year. Enterprise packs
add ten automations for EUR 10/month or EUR 100/year. Another 1,000 executions
will cost EUR 5, with customer approval, plus AI usage. These execution quotas
must be validated with load tests before launch. Prices assume shared
infrastructure with customer isolation. Dedicated hosting and availability
commitments require a separate quote. Hosting, backups and managed updates
are planned subscription features.

AI is separate: bring your own key and pay the provider directly, or pay Diffrook
monthly for managed tokens consumed. Annual discounts do not discount AI.
Proposed Diffrook sale rates per one million tokens:

| Model | Input EUR | Output EUR |
| --- | ---: | ---: |
| Claude Sonnet 5.5 | 3 | 15 |
| Claude Opus 5.5 | 6 | 30 |

These are proposed sale prices, not provider prices. Cache will have separate
rates. Monthly AI budgets, alerts and stopping consumption at the budget limit
are planned. Without caching, two million Sonnet input tokens and 200,000 output
tokens cost EUR 9; Freelancer Cloud then totals EUR 28 that month before tax.

## Obtain and install a paid self-hosted license

1. Open **Settings** and copy the **Installation ID**, persisted in SQLite.
   For a fresh SSO-only installation, the login screen and `GET /api/status`
   expose this non-secret ID before login.
2. In **Plans and pricing**, select monthly or annual billing and, for Enterprise,
   at least ten seats and any extra automation packs. Download **Prepare license
   request**. Visit [diffrook.dev/buy](https://diffrook.dev/buy/), import the request
   or enter the Installation ID and select the plan. Enter the licence holder name.
   The JSON is a request and quote, not a license or receipt. Purchases remain
   disabled while payment configuration is incomplete. On an SSO-only instance,
   use the Installation ID displayed before login.
3. Save the private licence link before continuing to Stripe Checkout. After
   confirmed payment, this link provides the signed file and a billing portal.
   Keep it private: anyone with it can download the licence and manage billing.
4. Download the signed license, mount it read-only and set `DIFFROOK_LICENSE_FILE`
   to its container path. Restart and check Settings.

When online checkout is enabled, Stripe charges the monthly or annual subscription
until cancelled. Confirmed renewal payments produce a new signed file at the same
private link. Download each renewal, replace the mounted file and restart Diffrook
before the previous file expires. The self-hosted application does not connect to
Stripe or fetch renewals. Delivery is through the private download page, not email.
The billing portal allows payment method updates, invoice downloads and cancellation
at the end of the paid period. Plan changes and installation transfers require the
owner. Lost private links require payment verification by the owner before recovery.
Manual agreements and owner-issued files remain available. Legacy signed
Business v1 files retain their original allowances and expiry, including
previously issued perpetual files. New v2 subscriptions require a billing
period, expiry and exact plan allowances.

Example Compose overlay:

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
the compiled Ed25519 public key. Invalid signatures, wrong installation IDs,
malformed claims, incorrect plan quotas and Cloud claims fail startup. A valid
expired or missing file uses Individual creation limits. Expiry is checked on
each operation. No licensing server or telemetry is used. No environment switch
activates a plan.

## SSO access and one-user installations

New SSO identities require an active Teams or Enterprise subscription.
Individual and Freelancer use local sign-in. OIDC configuration alone does not activate SSO.
Previously provisioned identities can still sign in through the configured IdP
after expiry, downgrade or upgrade, with signature and current allowlist checks, to retain
access to data and Settings. New identities are blocked until a Teams or Enterprise license is active. Existing identities do not consume another slot.

Local and SSO identities are separate; they are never automatically linked by
username or email. A local account uses a user slot. A fresh SSO-only installation
requires Teams or Enterprise: configure OIDC with local login disabled, obtain
its Installation ID from the login screen, install the signed license and restart.
The first allowlisted SSO identity creates an account. An existing local account
can remain alongside SSO users within the plan's user allowance.

Local setup creates only the initial administrator; additional users enter via
SSO. All users have administrator access. Seat allowances do not introduce
viewer/member roles. Only allow intended administrators in the IdP and allowlist.

## Quotas, expiry and existing data

Automation and user creation check quotas atomically in SQLite, including
parallel requests. An edit cannot recreate a concurrently deleted automation.
Settings disables automation creation at the limit; the API independently enforces it.
Existing installations above a quota or with an expired license retain records,
login access and existing automation execution. Nothing is deleted or disabled.
Editing, deleting and exporting remain available. Creation is blocked for each
resource at its allowance. Reduce usage or renew. Retained technical access
does not extend contractual permission to operate commercially.

## Issue licenses as the project owner

The existing issuer key is preserved:

- `src/license-public-key.hex` is public and compiled into official builds.
- `secrets/license-signing-key.pem` is private and excluded from Git and Docker
  build context. Back it up securely. Never distribute it to customers.

Only initial setup with both files absent may use `node scripts/license.mjs init`.
It refuses to overwrite files. Key loss or rotation requires deliberate migration
of official builds and licenses.

For a request downloaded from Settings, first calculate the current quote without
reading the signing key:

```sh
node scripts/license.mjs quote --request customer-request.json
```

The result always reports `payment_verified: false`. The tool checks allowances
against the catalog and warns about outdated quoted prices. After independently
confirming payment and its expiry, issue a manual file:

```sh
node scripts/license.mjs issue --request customer-request.json --customer "Company name" --expires 2027-01-01T00:00:00Z --out ./secrets/customer-license.json
```

The website's automatic issuer is configured separately in the private
`diffrook-site` repository. It verifies Stripe webhook signatures, fetches canonical
paid invoices and subscriptions, calculates prices from this catalog and signs the
paid invoice period with the existing key. Test mode must use a separate issuer key
and database; test files must never be accepted by official production builds.

Annual Freelancer:

```sh
node scripts/license.mjs issue --plan freelancer --period annual --installation CUSTOMER_UUID --customer "Freelancer name" --out ./secrets/freelancer-license.json
```

Monthly Teams:

```sh
node scripts/license.mjs issue --plan teams --period monthly --installation CUSTOMER_UUID --customer "Company name" --out ./secrets/teams-license.json
```

Annual Enterprise, twenty seats plus two packs: twenty users, 220 automations
and EUR 1,700/year before tax:

```sh
node scripts/license.mjs issue --plan enterprise --period annual --users 20 --automation-packs 2 --installation CUSTOMER_UUID --customer "Company name" --out ./secrets/enterprise-license.json
```

`--plan` and `--period` are required. Expiry defaults to one calendar month or
year from issuance, clamping month-end days. Optional `--expires` specifies a
future ISO date/time matching the paid contract. Enterprise defaults to ten
seats and zero packs. Other plans reject seat or pack overrides. Cloud,
Individual and new generic Business files cannot be issued. The tool prints
the quote and does not collect payment.

V2 claims keep the existing `diffrook-license-v1` signature domain separator;
version, plan, period, expiry and quotas are signed. Old v1 Business files
remain verifiable. Files are created exclusively: use a new filename for a
renewal, mount it and restart. Keep license IDs and contracts. Backups retain
the Installation ID; clones do not grant extra licensed installations.

Customers control their servers and source; checks cannot prevent all source
or database changes. Official builds reject forged or edited license files.
There is no online revocation check. Treat issuer key loss as a security event.

## License terms

Read [LICENSE](../LICENSE). This project-specific text should receive legal review
before sales. Prior CC BY-NC 4.0 grants, Diffrook 1.0/1.1 grants and existing contracts
remain effective for covered material. Original texts remain in [docs/licenses](licenses/).
Third-party licenses still apply. Creative Commons explains that [prior CC grants
are irrevocable](https://creativecommons.org/faq/#what-if-i-change-my-mind-about-using-a-cc-license).

## Validation

The plan rollout passed 55 Rust tests, the owner CLI test, Clippy with warnings
treated as errors, the frontend production build and 16 integration smoke checks.
Tests cover paid quotas, concurrent user/automation creation, signed and expired
subscriptions, legacy Business files, SSO entitlement and existing identity
access on downgrade. Owner-issued Freelancer, Teams and Enterprise files were
also activated in a local server; removing the license preserved over-quota data.
Browser checks covered monthly/annual quotes, Enterprise packs, the downloaded
request, mobile layout and disabled Cloud actions. Docker Compose configuration
was checked. A local Docker image build was unavailable without a running daemon.
