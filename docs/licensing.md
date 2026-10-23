# Plans and self-hosted licenses

Diffrook currently runs on the customer's server. From v0.2.0-beta.2, Individual,
Freelancer and Teams are free during the beta program. Enterprise is excluded
and is not available for purchase while legal and fiscal setup is incomplete.
Individual remains free forever for personal, noncommercial use. Freelancer and
Teams permit professional and commercial self-hosted use during the beta.

## Activate a free beta plan

Open **Settings > Plans and pricing** and select **Use free during beta** for
Individual, Freelancer or Teams. The default is Individual. No card, payment,
subscription or signed license file is needed. Each plan keeps its normal user,
automation and SSO limits. Teams includes SSO; Freelancer does not.
The selection persists in SQLite and takes effect immediately. A downgrade
retains existing data and blocks additional creations above the new limit.
An existing active signed license retains precedence and its original terms.

For a fresh SSO-only instance, set `DIFFROOK_BETA_PLAN=teams` before the first
start. A selection saved in Settings takes precedence on later restarts.
Only Individual, Freelancer and Teams are accepted. Enterprise cannot be
activated with this setting. All SSO users remain administrators.

The beta authorization ends when the beta program ends with the first official
stable release. It does not include stable professional use, a perpetual paid
license or an automatic subscription. Stable builds ignore the stored beta
selection and `DIFFROOK_BETA_PLAN`: Freelancer and Teams require a separately
purchased signed license. Install that license before upgrading if you need to
retain paid allowances. Without it, stable builds use Individual allowances;
existing data is retained. The offline app does not contact a licensing server
to detect release publication. The beta's technical entitlement is removed by
upgrading to a stable build; the legal beta authorization remains limited to
the beta program. See [LICENSE section 3a](../LICENSE).

## Prices for the official release

The following subscription prices apply to paid licensing after the beta.
Prices are in EUR before applicable tax. Paid sales remain disabled until the
licensors confirm legal and fiscal readiness. Individual remains free forever.

| Self-hosted plan | Registered users | Saved automations | SSO | Monthly | Annual |
| --- | ---: | ---: | :---: | ---: | ---: |
| Individual | 1 | 3 | No | Free forever | Free |
| Freelancer | 1 | 10 | No | 5.99 | 59.90 |
| Teams | Purchased seats, 1 to 10 | 20 per purchased seat, pooled | Yes | 9.99/seat | 99.90/seat |
| Enterprise | Purchased seats, minimum 10 | 20 per purchased seat, pooled | Yes | Contact us | Contact us |

Enterprise pricing and Enterprise-only packs of ten extra automations are
arranged directly. Annual subscriptions cost ten monthly payments, paid up front.
Licences issued under the original 0.2 price list (Teams with 5 users and 20
automations, Enterprise with 10 automations per seat) remain valid with the
allowance they were signed with. Paid plans include
software updates during their term; response times and availability SLAs require
a separate agreement. Disabled automations count. Repository bot credentials
are connections, not Diffrook users.

All self-hosted plans provide their own server and AI provider credentials or
local model. There is no execution-based license charge; infrastructure and
AI bills are paid directly by the customer. The embedded [catalog](../src/plans.json)
is shared by the server, licence issuer and Settings. `GET /api/plans` returns only
public prices. Authenticated `GET /api/license` returns installation metadata
and usage. Sending prices or quotas in a request cannot activate a plan.

## Cloud Hosting: Coming soon

These are preview prices for the future Diffrook-hosted service. Cloud cannot be
purchased or activated; the issuer cannot create Cloud licenses. Managed AI
metering, hosted operations and billing are not available. All hosted plans are paid.

| Planned Cloud plan | Users | Saved automations | SSO | Monthly | Annual | Executions/month |
| --- | ---: | ---: | :---: | ---: | ---: | ---: |
| Individual | 1 | 3 | No | 5.99 | 59.90 | 500 |
| Freelancer | 1 | 10 | No | 9.99 | 99.90 | 2,000 |
| Teams | 1 to 10 seats | 20 per seat, pooled | Yes | 14.99/seat | 149.90/seat | 2,000/seat |
| Enterprise | Minimum 10 | 20 per seat, pooled | Yes | Contact us | Contact us | 2,000/seat |

Cloud Enterprise and its automation packs are priced by arrangement. Another 1,000 executions
will cost EUR 5, with customer approval, plus AI usage. These execution quotas
must be validated with load tests before launch. Prices assume shared
infrastructure with customer isolation. Dedicated hosting and availability
commitments require a separate quote. Hosting, backups and managed updates
are planned subscription features.

AI is separate: bring your own key and pay the provider directly, or pay Diffrook
monthly for managed tokens consumed. Annual discounts do not discount AI.
Managed AI is deliberately priced at twice Anthropic's API list price: using your
own provider key costs less and is what we recommend, while managed access is for
teams that want the simplest setup. Proposed rates per one million tokens, converted
from Anthropic's USD list price at the ECB reference rate of 2026-10-22:

| Model | Input EUR | Output EUR |
| --- | ---: | ---: |
| Claude Sonnet 5.5 | 3.57 | 17.85 |
| Claude Opus 5.5 | 7.14 | 35.70 |

The EUR amounts follow the exchange rate and Anthropic's list price at launch.
Cache will have separate rates. Monthly AI budgets, alerts and stopping consumption
at the budget limit are planned. Without caching, two million Sonnet input tokens
and 200,000 output tokens cost EUR 10.71; Freelancer Cloud then totals EUR 20.70
that month before tax.

## Obtain and install a paid self-hosted license

1. Open **Settings** and copy the **Installation ID**, persisted in SQLite.
   For a fresh SSO-only installation, the login screen and `GET /api/status`
   expose this non-secret ID before login.
2. In **Plans and pricing**, select monthly or annual billing and, for Teams,
   the number of seats (1 to 10). Enterprise is arranged directly. Download
   **Prepare license request**. Visit [diffrook.dev/buy](https://diffrook.dev/buy/), import the request
   or enter the Installation ID and select the plan. Enter the licence holder name.
   The JSON is a request and quote, not a license or receipt. Purchases remain
   disabled until legal and fiscal setup is complete and the licensors reopen sales. On an SSO-only instance,
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
licensors. Lost private links require payment verification by the licensors before recovery.
The manual issuer is retained for future authorized paid sales; new paid purchases are currently disabled. Legacy signed
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

New SSO identities require the free Teams beta entitlement or an active signed Teams or Enterprise subscription.
Individual and Freelancer use local sign-in. OIDC configuration alone does not activate SSO.
Previously provisioned identities can still sign in through the configured IdP
after expiry, downgrade or upgrade, with signature and current allowlist checks, to retain
access to data and Settings. New identities require an eligible Teams beta entitlement or an active Teams or Enterprise license. Existing identities do not consume another slot.

Local and SSO identities are separate; they are never automatically linked by
username or email. A local account uses a user slot. A fresh SSO-only installation
requires Teams or Enterprise. During beta, bootstrap with `DIFFROOK_BETA_PLAN=teams`.
For paid stable releases, configure OIDC with local login disabled, obtain
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

## Issue licenses as a licensor

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

Monthly Teams for five seats: 100 automations and EUR 49.95/month before tax:

```sh
node scripts/license.mjs issue --plan teams --period monthly --users 5 --installation CUSTOMER_UUID --customer "Company name" --out ./secrets/teams-license.json
```

Annual Enterprise, twenty seats plus two packs: twenty users and 420
automations, at the price agreed with the customer:

```sh
node scripts/license.mjs issue --plan enterprise --period annual --users 20 --automation-packs 2 --installation CUSTOMER_UUID --customer "Company name" --out ./secrets/enterprise-license.json
```

`--plan` and `--period` are required. Expiry defaults to one calendar month or
year from issuance, clamping month-end days. Optional `--expires` specifies a
future ISO date/time matching the paid contract. Teams defaults to one
seat and accepts up to ten; Enterprise defaults to ten seats and zero packs.
Freelancer rejects seat overrides and only Enterprise accepts packs. Cloud,
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

Diffrook is licensed jointly by Alexandre Ramos, Diogo Mariano and Guilherme
Pereira. Read [LICENSE](../LICENSE). This project-specific text should receive legal review
before sales. Prior CC BY-NC 4.0 grants, Diffrook 1.0 to 1.3 grants and existing contracts
remain effective for covered material. Original texts remain in [docs/licenses](licenses/).
Third-party licenses still apply. Creative Commons explains that [prior CC grants
are irrevocable](https://creativecommons.org/faq/#what-if-i-change-my-mind-about-using-a-cc-license).

## Validation

The plan rollout passed 55 Rust tests, the licence issuer CLI test, Clippy with warnings
treated as errors, the frontend production build and 16 integration smoke checks.
Tests cover paid quotas, concurrent user/automation creation, signed and expired
subscriptions, legacy Business files, SSO entitlement and existing identity
access on downgrade. Locally issued Freelancer, Teams and Enterprise files were
also activated in a local server; removing the license preserved over-quota data.
Browser checks covered monthly/annual quotes, Enterprise packs, the downloaded
request, mobile layout and disabled Cloud actions. Docker Compose configuration
was checked. A local Docker image build was unavailable without a running daemon.
