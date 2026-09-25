# Diffrook implementation contract

Diffrook is a self-hosted repository automation service. English UI and documentation. Rust backend, React/TypeScript frontend, SQLite storage, one Docker container plus a persistent `/data` volume. The project owner selected CC BY-NC 4.0; describe the project as source-available, not OSI open source.

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
- `GET /api/dashboard` -> `{automations: number, active_automations: number, runs: number, findings: number, recent_runs: Run[]}`.
- `GET /api/connections`, `GET /api/providers`, `GET /api/automations`, `GET /api/runs` -> arrays of objects.
- `POST /api/connections`, `/api/providers`, `/api/automations` creates; `PUT /api/{collection}/{id}` updates; `DELETE /api/{collection}/{id}` deletes. Return saved object for create/update.
- `POST /api/connections/{id}/test`, `/api/providers/{id}/test` -> `{ok:bool,message:string}`. Test real endpoints, not a fake success.
- `GET /api/runs/{id}` -> Run including output.
- `POST /api/automations/{id}/run` -> `{kind:"pull_request"|"issue"|"repository", repository:"owner/repo", number?:123, branch?:"main"}` queues run -> Run.
- `POST /api/runs/{id}/cancel` cancels queued/running run. `POST /api/runs/{id}/retry` queues new run.
- `POST /api/schedule/preview` -> `{cron:"0 9 * * 1",timezone:"Europe/Lisbon"}` returns `{next:[ISO date strings]}`.
- `POST /api/webhooks/{connection_id}` receives signed GitHub/Forgejo webhook. No session auth; require HMAC signature. Do not trigger own bot events or commands from unauthorized actors.
- `GET /healthz` public health response.

## Configuration JSON

Every stored object has `id`, `created_at`, `updated_at`. Core encrypts sensitive fields at rest and does not return plaintext secrets. GET responses have blank secret values plus `has_token`, `has_api_key`, `has_webhook_secret`, `has_url` flags. Empty secret in updates preserves current value. Full decrypted records reach engine only. Connections and providers are selected using ids.

Connection: `{id,name,kind:"github"|"forgejo",base_url,token,webhook_secret,bot_username}`. GitHub `base_url` is API base, default `https://api.github.com`; Forgejo is instance root, engine appends `/api/v1`. Test token through `/user`.

Provider: `{id,name,kind:"openai"|"ollama"|"anthropic",base_url,api_key,default_model}`. OpenAI base includes `/v1`, Ollama root default `http://host.docker.internal:11434`, Anthropic root `https://api.anthropic.com`. Per-automation model overrides default.

Automation: `{id,name,description,enabled,connection_id,provider_id,model,repositories:["owner/repo"],trigger:{events:["pull_request.opened","pull_request.synchronize","pull_request.ready_for_review","issues.opened","issues.labeled","issue_comment.command"],command:"/diffrook",schedule_enabled:false,cron:"0 9 * * 1",timezone:"UTC",schedule_target:"repository"|"open_pull_requests"|"open_issues",branch:"main"},filters:{labels:[],ignore_drafts:true,allowed_actors:[],ignore_paths:["vendor/**","dist/**","**/*.lock"]},action:"review"|"fix_pr"|"solve_issue"|"audit",instructions:"",limits:{max_files:200,max_file_bytes:64000,max_context_chars:180000,max_output_tokens:6000,timeout_seconds:600,max_fix_files:10},notifications:[{kind:"pr_comment"|"issue_comment"|"discord"|"slack"|"teams"|"webhook",url:""}],fix:{mode:"new_branch"|"existing_branch",branch_prefix:"diffrook/"}}`.

UI must make review/comment versus fix action explicit. Scheduled issues/PRs enumerate open matching items; repository audits target configured branch. No auto-merge. No arbitrary code execution inside main container. Fixes use structured file changes and SCM API commits. CI executes tests after publication; output explicitly states tests were not run locally. Root codebase is available through repository tree and files, with bounded multi-pass analysis and explicit coverage reporting. No universal 'safe to merge' claim.

Run: `{id,automation_id,automation_name,status:"queued"|"running"|"succeeded"|"failed"|"cancelled",trigger:object,created_at,started_at,finished_at,error,output:object|null}`. Output has summary, findings, logs, artifacts and usage. Deduplicate webhook deliveries and schedules; serialize mutating jobs to avoid branch races; avoid bot-triggered loops. Running cancellation should stop engine future.

## Engine boundary

`crate::engine::execute(ctx: crate::types::EngineContext) -> anyhow::Result<crate::types::RunOutput>`.

`crate::scm::test_connection(connection: &serde_json::Value, client: &reqwest::Client) -> anyhow::Result<String>`.

`crate::providers::test_provider(provider: &serde_json::Value, client: &reqwest::Client) -> anyhow::Result<String>`.

EngineContext contains `run_id`, decrypted `automation`, `connection`, `provider`, `trigger`, `client`. Engine modules don't import core/database modules. Core resolves configuration and wraps execution in timeout/cancellation. All untrusted code/comments are data, not authority to invoke actions. Enforce action scope in code and never send credential fields to models. HTTP redirects disabled on credential-bearing requests. No git hooks or user commands executed.

## Validation

Meaningful tests for webhook authenticity, authorization, schedule timezone behavior, provider response parsing, context bounds, file path validation, duplicate events, secrets redaction and patch publication. Mock network services for end-to-end flows without contacting user repositories. Build frontend, run cargo fmt/check/test/clippy, Docker build and container smoke test when available.
