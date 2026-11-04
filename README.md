# Diffrook

Self-hosted code review and repository automations for GitHub, GitLab, Bitbucket (Cloud and Data Center), Forgejo and Gitea. Choose when an automation runs, which model it uses, whether it reviews or writes a fix, and where it posts the result.

Diffrook uses a Rust service, a React web interface, and SQLite. The application runs in one Docker container with a persistent data volume. Connect it to an existing Ollama instance or an OpenAI-compatible or Anthropic endpoint.

The dashboard checks GitHub releases, including pre-releases, every 15 minutes. Choose manual installation, including pre-releases, or automatic in-container updates to stable releases only. Automatic updates are supported on Linux x86-64 and ARM64 Docker deployments. Each release publishes a matching update package; Diffrook verifies GitHub's SHA-256 asset digest, stages the package in `/data`, and restarts gracefully after active runs have finished. It rolls back to the image version if the updated process fails before the server starts. Back up `/data` before installing an update. Other platforms can inspect releases and update through their normal container workflow.

This is **v1.0.0, the first official stable release**. The self-hosted Individual plan is free and available by default. Paid self-hosted plans and all Cloud plans are Coming soon. The free Freelancer and Teams beta offer has ended;
new paid-plan selection and checkout are unavailable. Existing Individual usage
rights and signed licences remain valid. Read [release and upgrade instructions](docs/releases.md)
and the [security review and verification](docs/security.md) before upgrading.

The November 2026 security audit covered the self-hosted version of Diffrook.
Diffrook Cloud and its hosted infrastructure were outside the audit scope.

## Run with Docker

```sh
docker compose up --build -d
docker compose logs diffrook
```

Open `http://localhost:8080`. Use the setup token printed in the logs to create the first administrator. The setup token is not an AI API key. Provider keys and repository tokens are entered in the web interface after setup.

The included Compose file binds to localhost. Copy `.env.example` to `.env` to change the address or port. For remote access, put the service behind an HTTPS reverse proxy and set `DIFFROOK_SECURE_COOKIES=true`. Proxy requests and webhooks to port 8080.

The container runs as UID 10001 without a Docker socket, privileged access, or a separate database service. A bind-mounted data directory must be writable by UID 10001. The named volume in the example handles this automatically.

## Configure an automation

For company SSO and Internet access, see [SSO and HTTPS deployment](docs/sso-and-internet.md).
Each installation configures its own hostname and OpenID Connect provider, with an
explicit administrator allowlist. SSO defaults to disabling password login.

1. Create a code host connection with a bot account token and webhook secret. See [code hosts](#code-hosts).
2. Create an AI provider and test the connection.
3. Add reusable destinations in **Notifications**. Discord, Slack, Teams and generic webhook URLs are encrypted at rest and can be test-sent from this page.
4. Create an automation. Select repositories, model, trigger, filters, action, limits and notification destinations.
5. Configure a webhook in the repository that posts to `https://YOUR-HOST/api/webhooks/CONNECTION_ID`, or use scheduled and manual runs.
6. Inspect each run in the web interface. Findings include the location, explanation, severity and suggested correction.

### Code hosts

| Type | URL | Token | Repository format | Webhook |
| --- | --- | --- | --- | --- |
| GitHub | `https://api.github.com`, or `https://HOST/api/v3` for Enterprise Server | Personal access token | `owner/repo` | Content type `application/json`, secret set. Events: pull requests, issues, issue comments |
| GitLab | `https://gitlab.com` or the self-managed root | Personal, project or group token with `api` scope | `group/repo` or `group/subgroup/repo` | Secret token set. Triggers: merge request, issue and comment events |
| Bitbucket Cloud | `https://api.bitbucket.org` | Workspace or repository access token, or `email:API token` | `workspace/repo` | Secret set. Triggers: pull request created, updated and comment created; issue created, updated and comment created |
| Bitbucket Data Center | Instance root | HTTP access token with repository write | `PROJECT/repo` or `~user/repo` | Secret set. Events: pull request opened, source branch updated and comment added |
| Forgejo | Instance root | Access token with repository and issue scopes | `owner/repo` | Forgejo webhook, content type `application/json`, secret set |
| Gitea | Instance root | Access token with repository and issue scopes | `owner/repo` | Gitea webhook, content type `application/json`, secret set |

GitLab merge requests and Bitbucket pull requests are handled as pull requests. Bitbucket Data Center has no issue tracker, so issue automations are not available there. Bitbucket Cloud issues have no labels; their kind and component are used as labels for filters. Bitbucket sends one event for both new commits and pull request edits, so a review can also run after an edit.

Comment commands (`/diffrook`) need an allowed-actors list on every host except GitHub, because only GitHub reports whether the commenter is a repository member.

Typical automations include reviewing a newly opened PR, auditing a branch every Monday at 09:00 in a selected timezone, and creating a fix PR for an issue carrying a selected label.

After the first automation is saved, the app shows a one-time invitation to star
Diffrook on GitHub. It is optional and can be dismissed. The app never stars a
repository on your behalf.

Each automation has its own model. Models must follow structured-output instructions for findings and file changes. Provider compatibility and review quality depend on the configured model. The application never treats model output as permission to enable a write action.

## Reviews and fixes

Reviews use the repository tree and file contents to inspect changes in context. Runs record coverage and skipped content when configured limits are reached. A completed review is not proof that a merge is safe.

Comment-only automations do not write code. Fix automations can create a branch and PR, or update an existing PR branch when explicitly configured. Diffrook does not merge PRs automatically.

The initial execution mode prepares file changes through the platform APIs. It does not run repository scripts, install dependencies, or execute generated code inside the application container. Configure the repository's CI to build and test proposed changes. Run output distinguishes analysis from test verification.

## Storage and backups

Configuration, sessions, run history and the encryption key live under `/data`. Back up the entire volume, including the key. Encrypted credentials cannot be restored without the matching key.

Stop the application before copying the volume so the SQLite database and its write-ahead log stay consistent:

```sh
docker compose stop diffrook
docker run --rm -v YOUR_VOLUME_NAME:/data:ro -v "$PWD/backups:/backup" alpine tar czf /backup/diffrook-data.tgz -C /data .
docker compose start diffrook
```

Use `docker volume ls` to find the actual Compose volume name. Keep backups private because they include the encryption key. Restore to an empty volume while the application is stopped and preserve file ownership.

## Outbound network access

Connections, AI providers and notification webhooks may contact public HTTP(S)
destinations by default. Diffrook blocks private, loopback, link-local, metadata
and reserved addresses, including IPv4 mapped into IPv6. Every request resolves
DNS, rejects any non-public answer and pins the checked addresses for that
connection. Automatic redirects and environment HTTP proxies are disabled.
Existing saved integrations use the same checks when they run.

For a private code host or local Ollama, the installation operator must set
`DIFFROOK_OUTBOUND_PRIVATE_HOSTS` to exact, comma-separated hostnames or IPs.
For example, `host.docker.internal,git.internal.example` permits those hosts
to reach private addresses on any port. Wildcards and CIDR ranges are not
supported. This setting grants network access to those hosts, so keep the
list narrow. Docker Compose passes the setting from `.env`; restart the
application after changing it. With an empty list, local Ollama is blocked.

API object writes reject unknown fields, including fields inside automation
settings and notifications. Editor response metadata is accepted for round
trips but discarded; identifiers and timestamps are assigned by the server.

## Development

Install a current stable Rust toolchain and Node.js 22 or later.

```sh
cd web
npm ci
npm run build
cd ..
cargo run
```

Use `DIFFROOK_WEB_DIR=web/dist` and `DIFFROOK_DATA_DIR=data` for a local run if needed. For frontend hot reload, run `npm run dev` in `web` while the Rust API is running.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

See [the architecture](docs/architecture.md) and [the implementation contract](IMPLEMENTATION.md) for the service boundaries and API schema.

## Project status

This is the first implementation. GitHub, GitLab, Bitbucket Cloud, Bitbucket Data Center, Forgejo and Gitea are supported. An isolated command runner can be added through the existing boundaries.

## License

Diffrook is source-available under the [Personal and Business License](LICENSE).
The self-hosted Individual plan is free and available by default. Paid self-hosted plans and all Cloud plans are Coming soon. Prices and allowances in the
catalog are previews. The free Freelancer and Teams beta offer has ended;
v1.0.0 ignores stored beta selections and DIFFROOK_BETA_PLAN. Existing Individual
usage rights and signed customer licences remain effective. Previously granted
rights are not revoked. See [plans and licences](docs/licensing.md).
