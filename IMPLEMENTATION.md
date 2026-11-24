# Diffrook implementation contract

Diffrook is a self-hosted repository automation service. English UI and documentation. Rust backend, React/TypeScript frontend, SQLite storage, one Docker container plus a persistent `/data` volume. It is licensed by Alexandre Ramos, Diogo Mariano and Guilherme Pereira under the Diffrook Personal and Business License (`LICENSE`); describe the project as source-available, not OSI open source.

## Ownership

- Core agent: `src/main.rs`, `src/core.rs`, `src/auth.rs`, `src/db.rs`, `src/routes.rs`, `src/scheduler.rs`, migrations and core tests.
- Engine agent: `src/engine.rs`, `src/providers.rs`, `src/scm.rs`, `src/notifications.rs` and engine tests.
- UI agent: all `web/` sources and UI tests.
- Coordinator: Cargo manifest, `src/types.rs`, Docker/Compose, docs, integration tests and integration fixes.

## HTTP API contract

All APIs use JSON. Errors are `{ "error": "human-readable message" }`. Authentication is an HttpOnly SameSite session cookie. Mutating authenticated requests must include `X-Diffrook-Request: 1`. No open registration after initial setup.

- `GET /api/status` -> `{setup_required: bool, authenticated: bool, version: string}`.
- `POST /api/setup` -> `{username,password,setup_token}`; creates first admin and sets session. Setup token is generated on first boot and logged, or configured by env. Require it to prevent remote first-user takeover.
- `POST /api/login` -> `{username,password}`, sets session. `POST /api/logout`.
- `GET /api/dashboard` -> `{automations: number, active_automations: number, connection_configured: bool, runs: number, completed_runs: number, failed_runs: number, automations_used: number, findings: number, prs_analyzed: number, prs_last_7d: number, tokens: {input, output, total, runs_with_usage, model_runs}, weekly_severities: {level: number}, weekly: [{date, completed, failed, findings, tokens, measured}] x 7, weekly_totals: {completed, failed, findings, tokens, runs_with_usage}, recent_runs: Run[]}`. Completed counts use the same `succeeded` status constant the queue writes; `automations_used` only counts automations that exist and started a run; `model_runs` counts the runs that could have spent anything (`succeeded`, `failed` or `cancelled`), `runs_with_usage` and the per-day `measured` count those that actually reported provider usage, and token sums are taken over `model_runs` only, so an unmeasured week is never drawn as a week that spent nothing and "measured in N of M runs" can never claim more than M; findings follow the runs that reached the success status; chart days are UTC and keyed on `finished_at` falling back to `created_at`; `weekly_severities` follows the chart window, not all time. Metrics are computed by scanning stored runs, which is acceptable at self-hosted volume but is the first thing to cache or aggregate if an instance grows.
- `GET /api/updates` -> `{current_version, releases: [{tag, version, prerelease, newer, supported, unsupported_reason, rolled_back, install_failures, asset_url, asset_digest, asset_size}], checked_at, last_attempt, error, retry_after, install_error: {tag, version, message, attempts, counted, at}, pending_install, pending_version, policy, channel, latest}`. Tags are GitHub's; `version` fields carry the bare version because the page prefixes its own `v`. A release is `supported` only when its package passes the checks the installer itself makes (HTTPS asset on GitHub, a `sha256:` digest, a size inside the limit), so an offered Install is always an installable one. `POST /api/updates/check` re-runs the GitHub check on demand, throttled on the last attempt (success or failure), sharing one request between concurrent clicks, honouring a wait GitHub asked for, and returning `restarting`/`deferred`/`install_error`. `POST /api/updates/policy` and `/api/updates/channel` save first and report those fields plus `dropped_pending`, instead of failing when the immediate install fails. `POST /api/updates/apply {tag}` installs a release on the current channel; while automations are running it stores the full release manifest as `updates.pending_install` and returns `{ok:true,queued:true}`, and a worker independent of the release feed performs it when the runs finish, even after a restart or during a GitHub outage. `POST /api/updates/cancel` drops that queued request. The queue is revalidated before it is carried out (still newer than the running version, still on the chosen channel, still installable) and a switching channel drops it explicitly; the worker writes it back only while the stored request is still the one it read, so a retry can never resurrect a cancelled or replaced install. Automatic installs count only failures that will repeat (bad checksum, corrupt or unusable package), and three within six hours park that release until an administrator retries.
- `GET /api/connections`, `GET /api/providers`, `GET /api/automations`, `GET /api/runs` -> arrays of objects.
- `POST /api/connections`, `/api/providers`, `/api/automations` creates; `PUT /api/{collection}/{id}` updates; `DELETE /api/{collection}/{id}` deletes. Return saved object for create/update.
- `POST /api/connections/{id}/test`, `/api/providers/{id}/test` -> `{ok:bool,message:string}`. Test real endpoints, not a fake success.
- `GET /api/runs/{id}` -> Run including output.
- `POST /api/automations/{id}/run` -> `{kind:"pull_request"|"issue"|"repository", repository:"owner/repo", number?:123, branch?:"main"}` queues run -> Run.
- `POST /api/runs/{id}/cancel` cancels queued/running run. `POST /api/runs/{id}/retry` queues new run.
- `POST /api/schedule/preview` -> `{cron:"0 9 * * 1",timezone:"Europe/Lisbon"}` returns `{next:[ISO date strings]}`.
- `POST /api/webhooks/{connection_id}` receives code host webhooks. No session auth; require the host's HMAC signature, or the shared secret token for GitLab. Do not trigger own bot events or commands from unauthorized actors.
- `GET /healthz` public health response.

## Configuration JSON

Every stored object has `id`, `created_at`, `updated_at`. Core encrypts sensitive fields at rest and does not return plaintext secrets. GET responses have blank secret values plus `has_token`, `has_api_key`, `has_webhook_secret`, `has_url` flags. Empty secret in updates preserves current value. Full decrypted records reach engine only. Connections and providers are selected using ids.

Connection: `{id,name,kind:"github"|"gitlab"|"bitbucket"|"bitbucket_datacenter"|"forgejo"|"gitea",base_url,token,webhook_secret,bot_username}`. GitHub `base_url` is API base, default `https://api.github.com`. GitLab defaults to `https://gitlab.com` and the engine appends `/api/v4`. Bitbucket Cloud defaults to `https://api.bitbucket.org` and appends `/2.0`. Bitbucket Data Center, Forgejo and Gitea need the instance root; the engine appends `/rest/api/latest` or `/api/v1`. Test the token through the host's identity endpoint.

Provider: `{id,name,kind:"openai"|"ollama"|"anthropic",base_url,api_key,default_model}`. OpenAI base includes `/v1`, Ollama root default `http://host.docker.internal:11434`, Anthropic root `https://api.anthropic.com`. Per-automation model overrides default.

Automation: `{id,name,description,enabled,connection_id,provider_id,model,repositories:["owner/repo"],trigger:{events:["pull_request.opened","pull_request.synchronize","pull_request.ready_for_review","issues.opened","issues.labeled","issue_comment.command"],command:"/diffrook",schedule_enabled:false,cron:"0 9 * * 1",timezone:"UTC",schedule_target:"repository"|"open_pull_requests"|"open_issues",branch:"main"},filters:{labels:[],ignore_drafts:true,allowed_actors:[],ignore_paths:["vendor/**","dist/**","**/*.lock"]},action:"review"|"fix_pr"|"solve_issue"|"audit",instructions:"",limits:{max_files:200,max_file_bytes:64000,max_context_chars:180000,max_output_tokens:6000,timeout_seconds:600,max_fix_files:10},notifications:[{kind:"pr_comment"|"issue_comment"|"discord"|"slack"|"teams"|"webhook",url:""}],fix:{mode:"new_branch"|"existing_branch",branch_prefix:"diffrook/"}}`.

UI must make review/comment versus fix action explicit. Scheduled issues/PRs enumerate open matching items; repository audits target configured branch. No auto-merge. No arbitrary code execution inside main container. Fixes use structured file changes and SCM API commits. CI executes tests after publication; output explicitly states tests were not run locally. Root codebase is available through repository tree and files, with bounded multi-pass analysis and explicit coverage reporting. No universal 'safe to merge' claim.

Run: `{id,automation_id,automation_name,connection_id,status:"queued"|"running"|"succeeded"|"failed"|"cancelled",trigger:object,created_at,started_at,finished_at,error,output:object|null,usage?:{input_tokens,output_tokens}}`. `connection_id` is stamped when the run is queued, which is how the overview tells two code hosts apart for the same repository. `usage` is written when a run failed after a provider had already answered, so spent tokens survive the failure. Output has summary, findings, logs, artifacts and usage. Deduplicate webhook deliveries and schedules; serialize mutating jobs to avoid branch races; avoid bot-triggered loops. Running cancellation should stop engine future.

## Engine boundary

`crate::engine::execute(ctx: crate::types::EngineContext) -> anyhow::Result<crate::types::RunOutput>`.

`crate::scm::test_connection(connection: &serde_json::Value, client: &reqwest::Client) -> anyhow::Result<String>`.

`crate::providers::test_provider(provider: &serde_json::Value, client: &reqwest::Client) -> anyhow::Result<String>`.

EngineContext contains `run_id`, decrypted `automation`, `connection`, `provider`, `trigger`, `client`. Engine modules don't import core/database modules. Core resolves configuration and wraps execution in timeout/cancellation. All untrusted code/comments are data, not authority to invoke actions. Enforce action scope in code and never send credential fields to models. HTTP redirects disabled on credential-bearing requests. No git hooks or user commands executed.

## Validation

Meaningful tests for webhook authenticity, authorization, schedule timezone behavior, provider response parsing, context bounds, file path validation, duplicate events, secrets redaction and patch publication. Mock network services for end-to-end flows without contacting user repositories. Build frontend, run cargo fmt/check/test/clippy, Docker build and container smoke test when available.
