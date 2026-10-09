# Diffrook

Self-hosted code review and repository automations for GitHub and Forgejo. Choose when an automation runs, which model it uses, whether it reviews or writes a fix, and where it posts the result.

Diffrook uses a Rust service, a React web interface, and SQLite. The application runs in one Docker container with a persistent data volume. Connect it to an existing Ollama instance or an OpenAI-compatible or Anthropic endpoint.

The dashboard checks GitHub releases, including pre-releases, every 15 minutes. Choose manual installation or automatic in-container updates. Automatic updates are supported on Linux x86-64 and ARM64 Docker deployments. Each release publishes a matching update package; Diffrook verifies GitHub's SHA-256 asset digest, stages the package in `/data`, and restarts gracefully after active runs have finished. It rolls back to the image version if the updated process fails before the server starts. Back up `/data` before installing an update. Other platforms can inspect releases and update through their normal container workflow.

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

1. Create a GitHub or Forgejo connection with a bot account token and webhook secret.
2. Create an AI provider and test the connection.
3. Add reusable destinations in **Notifications**. Discord, Slack, Teams and generic webhook URLs are encrypted at rest and can be test-sent from this page.
4. Create an automation. Select repositories, model, trigger, filters, action, limits and notification destinations.
5. Configure a webhook in the repository, or use scheduled and manual runs.
6. Inspect each run in the web interface. Findings include the location, explanation, severity and suggested correction.

Typical automations include reviewing a newly opened PR, auditing a branch every Monday at 09:00 in a selected timezone, and creating a fix PR for an issue carrying a selected label.

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

This is the first implementation. GitHub and Forgejo are the initial platforms. Additional repository platforms and an isolated command runner can be added through the existing boundaries.

## License

Copyright 2026 Alexandre Ramos and Diffrook contributors.

Diffrook is source-available under the [Diffrook Personal and Business License](LICENSE).
The free Individual plan permits personal, noncommercial self-hosted use with
**one user and three saved automations**, including disabled automations. Company,
team, employment, freelance and other professional use requires a paid plan,
even with one user. Paid self-hosted plans use offline signed, expiring licenses:

| Self-hosted plan | Users | Saved automations | SSO | Monthly | Annual |
| --- | ---: | ---: | :---: | ---: | ---: |
| Individual | 1 | 3 | No | Free forever | Free |
| Freelancer | 1 | 10 | No | EUR 9 | EUR 90 |
| Teams | 5 | 20 | Yes | EUR 29 | EUR 290 |
| Enterprise | Minimum 10 | 10 per licensed user, pooled | Yes | EUR 8/user | EUR 80/user |

Prices exclude applicable tax. Enterprise packs add ten automations for EUR 5/month
or EUR 50/year. All self-hosted plans bring their own AI and infrastructure;
there is no execution-based license charge. Previously issued Business licenses
remain supported. Settings shows the catalog and prepares a license request for
the project owner; payment and renewal are arranged directly.

**Cloud Hosting: Coming soon.** The catalog previews paid hosted subscriptions,
BYOK or managed AI billed monthly by token consumption. Cloud purchase, activation
and managed AI billing are unavailable. Hosted subscriptions will have separate
service terms. Third-party hosting or
resale requires separate written authorization. See [plans and license installation](docs/licensing.md).

These terms are not an OSI open-source license. Rights already granted for
earlier CC BY-NC 4.0 material remain intact; its [original license](docs/licenses/CC-BY-NC-4.0.txt)
is retained. Third-party dependencies keep their respective licenses.
