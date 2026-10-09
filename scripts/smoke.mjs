// Integration test against an isolated Diffrook instance. Never use a production instance.
// DIFFROOK_TEST_URL=http://localhost:18080 DIFFROOK_TEST_SETUP_TOKEN=... node scripts/smoke.mjs
import assert from 'node:assert/strict';
import http from 'node:http';
import { createHmac, randomUUID } from 'node:crypto';
import { writeFile, mkdir } from 'node:fs/promises';

const origin = process.env.DIFFROOK_TEST_URL || 'http://localhost:18080';
const mockPort = Number(process.env.DIFFROOK_MOCK_PORT || 19091);
const mockOrigin = process.env.DIFFROOK_MOCK_ORIGIN || `http://host.docker.internal:${mockPort}`;
const secret = 'integration-webhook-secret-not-a-real-credential';
const token = 'integration-repository-token-not-a-real-credential';
const modelKey = 'integration-model-key-not-a-real-credential';
const sha = '1'.repeat(40), nextSha = '2'.repeat(40);
const traffic = [], branches = new Map([['main', sha], ['feature/divide', sha]]);
let cookie = '';
const report = [];
let slow = false;

function modelResult(body) {
  const userMessage = body.messages?.findLast(m => m.role === 'user')?.content || '';
  const action = /Automation action:\s*(\w+)/.exec(userMessage)?.[1] || 'review';
  return { summary: 'A zero divisor can cause a runtime failure.', findings: [{ severity: 'high', title: 'Guard a zero divisor', file: 'src/math.rs', line: 2, explanation: 'Integer division panics when the divisor is zero.', suggestion: 'Return an error when divisor is zero.' }], changes: ['solve_issue', 'fix_pr'].includes(action) ? [{ path: 'src/math.rs', content: 'pub fn divide(a: i32, b: i32) -> Option<i32> { a.checked_div(b) }\n' }] : [] };
}

const mock = http.createServer(async (req, res) => {
  let raw = '';
  for await (const chunk of req) raw += chunk;
  let body;
  try { body = raw ? JSON.parse(raw) : null; } catch { body = raw; }
  const url = new URL(req.url, `http://localhost:${mockPort}`);
  const path = decodeURIComponent(url.pathname).replace(/^\/api\/v1/, '');
  traffic.push({ method: req.method, path, body, headers: { authorization: req.headers.authorization, signature: req.headers['x-hub-signature-256'] } });
  const send = (data, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(data)); };
  const pr = { number: 7, title: 'Handle division', body: 'Improve division behavior', state: 'open', draft: false, user: { login: 'developer' }, labels: [], head: { ref: 'feature/divide', sha: branches.get('feature/divide'), repo: { full_name: 'acme/demo' } }, base: { ref: 'main', sha, repo: { full_name: 'acme/demo' } }, html_url: 'https://example.invalid/acme/demo/pull/7' };
  if (path === '/user') return send({ login: 'diffrook-bot' });
  if (['/v1/models', '/models', '/api/tags'].includes(path)) return send({ data: [{ id: 'test-model' }], models: [{ name: 'test-model' }] });
  if (['/v1/chat/completions', '/api/chat', '/v1/messages'].includes(path)) {
    if (slow) await new Promise(r => setTimeout(r, 5000));
    const text = JSON.stringify(modelResult(body));
    return send(path.endsWith('/messages') ? { content: [{ type: 'text', text }], usage: { input_tokens: 100, output_tokens: 80 } } : path === '/api/chat' ? { message: { content: text }, prompt_eval_count: 100, eval_count: 80 } : { choices: [{ message: { content: text }, finish_reason: 'stop' }], usage: { prompt_tokens: 100, completion_tokens: 80 } });
  }
  if (path === '/notify') return send({ ok: true });
  if (path === '/repos/acme/demo' && req.method === 'GET') return send({ full_name: 'acme/demo', default_branch: 'main', html_url: 'https://example.invalid/acme/demo', fork: false });
  if (path === '/repos/acme/demo/pulls/7' && !req.headers.accept?.includes('diff')) return send(pr);
  if (path === '/repos/acme/demo/pulls/7.diff' || path === '/repos/acme/demo/pulls/7' && req.headers.accept?.includes('diff')) { res.writeHead(200, { 'content-type': 'text/plain' }); return res.end('diff --git a/src/math.rs b/src/math.rs\n--- a/src/math.rs\n+++ b/src/math.rs\n@@ -1 +1,2 @@\n-pub fn divide(a:i32,b:i32)->i32 { a/b }\n+pub fn divide(a: i32, b: i32) -> i32 {\n+    a / b }\n'); }
  if (path === '/repos/acme/demo/pulls' && req.method === 'GET') return send([pr]);
  if (path === '/repos/acme/demo/issues' && req.method === 'GET') return send([{ number: 9, title: 'Division by zero', labels: [{ name: 'autofix' }], user: { login: 'developer' } }]);
  if (path === '/repos/acme/demo/issues/9') return send({ number: 9, title: 'Division by zero', body: 'Please return an error for a zero divisor.', user: { login: 'developer' }, labels: [{ name: 'autofix' }], html_url: 'https://example.invalid/acme/demo/issues/9' });
  if (/\/issues\/\d+\/comments$/.test(path)) return send(req.method === 'GET' ? [] : { id: 50, html_url: 'https://example.invalid/comment/50' }, req.method === 'GET' ? 200 : 201);
  if (/\/collaborators\/[^/]+\/permission$/.test(path)) return send({ permission: 'write', user: { permissions: { push: true } } });
  if (path.startsWith('/repos/acme/demo/branches/') && req.method === 'GET') return send({ name: path.split('/branches/')[1], commit: { sha: branches.get(path.split('/branches/')[1]) || sha } });
  if (path === '/repos/acme/demo/branches' && req.method === 'POST') { branches.set(body.new_branch_name, sha); return send({ name: body.new_branch_name, commit: { sha } }, 201); }
  if (path.includes('/git/trees/') && req.method === 'GET') return send({ sha: 'tree-sha', truncated: false, tree: [{ path: 'src/math.rs', type: 'blob', mode: '100644', sha: 'file-sha', size: 65 }, { path: '.env', type: 'blob', mode: '100644', sha: 'secret-file', size: 40 }, { path: 'README.md', type: 'blob', mode: '100644', sha: 'readme-sha', size: 30 }] });
  if (path.includes('/git/commits/') && req.method === 'GET') return send({ sha, tree: { sha: 'tree-sha' } });
  if (path.includes('/contents/') && req.method === 'GET') {
    const file = path.split('/contents/')[1];
    if (file === '.env') return send({ content: Buffer.from('NEVER_SEND_THIS_SECRET=should-be-skipped').toString('base64'), encoding: 'base64', sha: 'secret-file' });
    return send({ path: file, content: Buffer.from(file === 'src/math.rs' ? 'pub fn divide(a: i32, b: i32) -> i32 { a / b }\n' : '# Demo\nInteger division library\n').toString('base64'), encoding: 'base64', sha: file === 'src/math.rs' ? 'file-sha' : 'readme-sha' });
  }
  if (path === '/repos/acme/demo/git/refs' && req.method === 'POST') { branches.set(body.ref.replace('refs/heads/', ''), body.sha); return send({ ref: body.ref, object: { sha: body.sha } }, 201); }
  if (path.endsWith('/git/trees') && req.method === 'POST') return send({ sha: 'new-tree-sha' }, 201);
  if (path.endsWith('/git/commits') && req.method === 'POST') return send({ sha: nextSha }, 201);
  if (path.includes('/git/refs/heads/') && req.method === 'PATCH') { branches.set(path.split('/git/refs/heads/')[1], body.sha); return send({ object: { sha: body.sha } }); }
  if (path === '/repos/acme/demo/contents' && req.method === 'POST') { branches.set(body.branch, nextSha); return send({ commit: { sha: nextSha }, files: body.files }, 201); }
  if (path === '/repos/acme/demo/pulls' && req.method === 'POST') return send({ number: 10, html_url: 'https://example.invalid/acme/demo/pull/10', ...body }, 201);
  console.error('Unhandled mock request:', req.method, path);
  return send({ error: 'Unhandled mock endpoint', path }, 404);
});

await new Promise(resolve => mock.listen(mockPort, '0.0.0.0', resolve));

async function api(path, method = 'GET', body, options = {}) {
  const headers = { 'content-type': 'application/json', 'x-diffrook-request': '1', ...(cookie ? { cookie } : {}), ...options.headers };
  const res = await fetch(origin + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  let data;
  try { data = text ? JSON.parse(text) : null; } catch { throw new Error(`${method} ${path}: not JSON: ${text.slice(0, 200)}`); }
  if (options.status) assert.equal(res.status, options.status, `${method} ${path}: ${text}`);
  else assert.ok(res.ok, `${method} ${path}: ${res.status} ${text}`);
  const setCookie = res.headers.get('set-cookie');
  if (setCookie) cookie = setCookie.split(';')[0];
  return data;
}
async function waitForRun(id) {
  for (let i = 0; i < 100; i++) {
    const run = await api(`/api/runs/${id}`);
    if (!['queued', 'running'].includes(run.status)) return run;
    await new Promise(r => setTimeout(r, 200));
  }
  throw new Error(`Run ${id} did not finish`);
}
async function waitForIdle() {
  for (let i = 0; i < 100; i++) {
    const runs = await api('/api/runs');
    if (runs.every(run => !['queued', 'running'].includes(run.status))) return;
    await new Promise(r => setTimeout(r, 100));
  }
  throw new Error('Background runs did not become idle');
}
async function check(name, fn) { await fn(); report.push(name); console.log(`PASS ${name}`); }
const automation = (connectionId, providerId, overrides = {}) => ({ name: 'Integration review', description: 'Local mock services only', enabled: true, connection_id: connectionId, provider_id: providerId, model: 'test-model', repositories: ['acme/demo'], trigger: { events: ['pull_request.opened', 'issue_comment.command'], command: '/diffrook', schedule_enabled: false, cron: '0 9 * * 1', timezone: 'Europe/Lisbon', schedule_target: 'repository', branch: 'main' }, filters: { labels: [], ignore_drafts: true, allowed_actors: ['developer'], ignore_paths: [] }, action: 'review', instructions: 'CHECK_AUTOMATION_INSTRUCTIONS_ARE_USED', limits: { max_files: 200, max_file_bytes: 64000, max_context_chars: 180000, max_output_tokens: 6000, timeout_seconds: 60, max_fix_files: 10 }, notifications: [{ kind: 'pr_comment', url: '' }], fix: { mode: 'new_branch', branch_prefix: 'diffrook/' }, ...overrides });
let gh, forgejo, provider, auto, notificationProvider;
try {
  await check('Setup and authentication', async () => {
    const status = await api('/api/status');
    assert.equal(status.setup_required, true, 'Use a fresh isolated database for smoke tests');
    await api('/api/connections', 'GET', undefined, { status: 401 });
    await api('/api/setup', 'POST', { username: 'tester', password: 'Diffrook-test-password-123', setup_token: process.env.DIFFROOK_TEST_SETUP_TOKEN || 'smoke-setup-token' });
    assert.ok(cookie.includes('diffrook_session='));
    assert.equal((await api('/api/status')).authenticated, true);
    await api('/api/setup', 'POST', { username: 'another', password: 'Diffrook-test-password-123', setup_token: process.env.DIFFROOK_TEST_SETUP_TOKEN || 'smoke-setup-token' }, { status: 409 });
  });
  await check('Manual and automatic update policy', async () => {
    const updates = await api('/api/updates');
    assert.equal(updates.current_version, (await api('/api/status')).version);
    assert.equal(updates.policy, 'manual');
    assert.ok(Array.isArray(updates.releases));
    assert.equal((await api('/api/updates/policy', 'POST', { mode: 'automatic' })).mode, 'automatic');
    assert.equal((await api('/api/updates', 'GET')).policy, 'automatic');
    assert.equal((await api('/api/updates/policy', 'POST', { mode: 'manual' })).mode, 'manual');
  });
  await check('CRUD, CSRF and masked credentials', async () => {
    await api('/api/connections', 'POST', {}, { headers: { 'x-diffrook-request': '' }, status: 403 });
    gh = await api('/api/connections', 'POST', { name: 'Mock GitHub', kind: 'github', base_url: mockOrigin, token, webhook_secret: secret, bot_username: 'diffrook-bot' });
    forgejo = await api('/api/connections', 'POST', { name: 'Mock Forgejo', kind: 'forgejo', base_url: mockOrigin, token, webhook_secret: secret, bot_username: 'diffrook-bot' });
    assert.equal(gh.token, ''); assert.equal(gh.has_token, true);
    assert.equal((await api(`/api/connections/${gh.id}/test`, 'POST', {})).ok, true);
    assert.equal((await api(`/api/connections/${forgejo.id}/test`, 'POST', {})).ok, true);
    provider = await api('/api/providers', 'POST', { name: 'Mock OpenAI', kind: 'openai', base_url: `${mockOrigin}/v1`, api_key: modelKey, default_model: 'test-model' });
    assert.equal(provider.api_key, ''); assert.equal(provider.has_api_key, true);
    assert.equal((await api(`/api/providers/${provider.id}/test`, 'POST', {})).ok, true);
    await api(`/api/connections/${gh.id}`, 'PUT', { ...gh, name: 'Mock GitHub edited' });
    assert.equal((await api(`/api/connections/${gh.id}/test`, 'POST', {})).ok, true);
    assert.equal(traffic.findLast(t => t.path === '/user').headers.authorization, `Bearer ${token}`);
  });
  await check('Five-field cron timezone preview', async () => {
    const preview = await api('/api/schedule/preview', 'POST', { cron: '0 9 * * 1', timezone: 'Europe/Lisbon' });
    assert.equal(preview.next.length, 5);
    for (const date of preview.next) {
      const parts = new Intl.DateTimeFormat('en-GB', { weekday: 'long', hour: '2-digit', minute: '2-digit', timeZone: 'Europe/Lisbon' }).format(new Date(date));
      assert.match(parts, /Monday/); assert.match(parts, /09:00/);
    }
  });
  await check('Reusable notification providers are masked and testable', async () => {
    notificationProvider = await api('/api/notification-providers', 'POST', { name: 'Mock Slack', kind: 'slack', url: `${mockOrigin}/notify?global=1` });
    assert.equal(notificationProvider.url, '');
    assert.equal(notificationProvider.has_url, true);
    assert.equal((await api(`/api/notification-providers/${notificationProvider.id}/test`, 'POST', {})).ok, true);
    assert.equal(traffic.findLast(t => t.path === '/notify').body.text.includes('test notification'), true);
    await api(`/api/notification-providers/${notificationProvider.id}`, 'PUT', { ...notificationProvider, name: 'Mock Slack edited' });
    assert.equal((await api('/api/notification-providers'))[0].has_url, true);
  });
  await check('GitHub PR review and contextual comment', async () => {
    auto = await api('/api/automations', 'POST', automation(gh.id, provider.id));
    const queued = await api(`/api/automations/${auto.id}/run`, 'POST', { kind: 'pull_request', repository: 'acme/demo', number: 7 });
    const run = await waitForRun(queued.id);
    assert.equal(run.status, 'succeeded', run.error);
    assert.equal(run.output.findings[0].title, 'Guard a zero divisor');
    assert.ok(traffic.some(t => t.method === 'POST' && t.path.endsWith('/issues/7/comments') && t.body.body.includes('Guard a zero divisor')));
    const modelBody = JSON.stringify(traffic.findLast(t => t.path === '/v1/chat/completions').body);
    assert.ok(modelBody.includes('CHECK_AUTOMATION_INSTRUCTIONS_ARE_USED'));
    assert.ok(!modelBody.includes(token) && !modelBody.includes(modelKey) && !modelBody.includes('NEVER_SEND_THIS_SECRET'));
    assert.ok(!traffic.some(t => t.method !== 'GET' && t.path.includes('/git/')));
  });
  await check('Individual plan limits automation creation to three', async () => {
    const first = await api('/api/automations', 'POST', automation(gh.id, provider.id, { name: 'Quota probe 1', enabled: false }));
    const second = await api('/api/automations', 'POST', automation(gh.id, provider.id, { name: 'Quota probe 2' }));
    await api('/api/automations', 'POST', automation(gh.id, provider.id), { status: 403 });
    const license = await api('/api/license');
    assert.equal(license.edition, 'individual');
    assert.equal(license.usage.users, 1);
    assert.equal(license.usage.automations, 3);
    assert.equal(license.can_create_automation, false);
    await api(`/api/automations/${first.id}`, 'DELETE');
    await api(`/api/automations/${second.id}`, 'DELETE');
  });
  await check('Forgejo review uses the Forgejo diff endpoint', async () => {
    const a = await api('/api/automations', 'POST', automation(forgejo.id, provider.id, { name: 'Forgejo review' }));
    const queued = await api(`/api/automations/${a.id}/run`, 'POST', { kind: 'pull_request', repository: 'acme/demo', number: 7 });
    const run = await waitForRun(queued.id);
    assert.equal(run.status, 'succeeded', run.error);
    assert.ok(traffic.some(t => t.path.endsWith('/pulls/7.diff')));
    await api(`/api/automations/${a.id}`, 'DELETE');
  });
  for (const c of [() => gh, () => forgejo]) {
    await check(`${c() === gh ? 'GitHub' : 'Forgejo'} issue to fix PR`, async () => {
      const a = await api('/api/automations', 'POST', automation(c().id, provider.id, { name: 'Solve labeled issue', action: 'solve_issue', notifications: [{ kind: 'issue_comment', url: '' }] }));
      const queued = await api(`/api/automations/${a.id}/run`, 'POST', { kind: 'issue', repository: 'acme/demo', number: 9 });
      const run = await waitForRun(queued.id);
      assert.equal(run.status, 'succeeded', run.error);
      assert.match(run.output.artifacts.pull_request_url, /pull\/10$/);
      const pr = traffic.findLast(t => t.method === 'POST' && t.path.endsWith('/pulls'));
      assert.equal(pr.body.base, 'main'); assert.match(pr.body.head, /^diffrook\//);
      await api(`/api/automations/${a.id}`, 'DELETE');
    });
  }
  for (const kind of ['ollama', 'anthropic']) {
    await check(`${kind} model adapter`, async () => {
      const p = await api('/api/providers', 'POST', { name: `Mock ${kind}`, kind, base_url: mockOrigin, api_key: modelKey, default_model: 'test-model' });
      assert.equal((await api(`/api/providers/${p.id}/test`, 'POST', {})).ok, true);
      const a = await api('/api/automations', 'POST', automation(gh.id, p.id, { name: `${kind} audit`, action: 'audit', notifications: [] }));
      const run = await waitForRun((await api(`/api/automations/${a.id}/run`, 'POST', { kind: 'repository', repository: 'acme/demo', branch: 'main' })).id);
      assert.equal(run.status, 'succeeded', run.error);
      await api(`/api/automations/${a.id}`, 'DELETE');
    });
  }
  await check('Webhook signature and replay protection', async () => {
    const payload = { action: 'opened', repository: { full_name: 'acme/demo' }, sender: { login: 'developer' }, pull_request: { number: 7, user: { login: 'developer' }, head: { ref: 'feature/divide' }, author_association: 'OWNER' } };
    const body = JSON.stringify(payload), id = randomUUID();
    const headers = { 'content-type': 'application/json', 'x-github-event': 'pull_request', 'x-github-delivery': id, 'x-hub-signature-256': 'sha256=' + createHmac('sha256', secret).update(body).digest('hex') };
    const bad = await fetch(`${origin}/api/webhooks/${gh.id}`, { method: 'POST', headers: { ...headers, 'x-hub-signature-256': 'sha256=bad' }, body });
    assert.equal(bad.status, 401);
    const good = await fetch(`${origin}/api/webhooks/${gh.id}`, { method: 'POST', headers, body }); assert.ok(good.ok); await good.json();
    const replay = await fetch(`${origin}/api/webhooks/${gh.id}`, { method: 'POST', headers, body }); assert.equal((await replay.json()).duplicate, true);
    const comment = JSON.stringify({ action: 'created', repository: { full_name: 'acme/demo' }, sender: { login: 'developer' }, issue: { number: 7, pull_request: {} }, comment: { body: 'An unrelated comment', user: { login: 'developer' }, author_association: 'OWNER' } });
    const noCommand = await fetch(`${origin}/api/webhooks/${gh.id}`, { method: 'POST', headers: { ...headers, 'x-github-event': 'issue_comment', 'x-github-delivery': randomUUID(), 'x-hub-signature-256': 'sha256=' + createHmac('sha256', secret).update(comment).digest('hex') }, body: comment });
    assert.equal((await noCommand.json()).queued, 0);
  });
  await check('Selected notification adapters and masked URL updates', async () => {
    await waitForIdle();
    const a = await api('/api/automations', 'POST', automation(gh.id, provider.id, { name: 'Notification audit', action: 'audit', notifications: [
      { kind: 'discord', url: `${mockOrigin}/notify?kind=discord` },
      { kind: 'slack', provider_id: notificationProvider.id },
      { kind: 'teams', url: `${mockOrigin}/notify?kind=teams` },
      { kind: 'webhook', url: `${mockOrigin}/notify?kind=webhook` },
    ] }));
    assert.equal(a.notifications[1].provider_id, notificationProvider.id);
    assert.ok(a.notifications[1].url === '');
    assert.ok(a.notifications.slice(0, 1).concat(a.notifications.slice(2)).every(n => n.url === '' && n.has_url));
    await api(`/api/automations/${a.id}`, 'PUT', { ...a, name: 'Notification audit edited' });
    const start = traffic.length;
    const run = await waitForRun((await api(`/api/automations/${a.id}/run`, 'POST', { kind: 'repository', repository: 'acme/demo', branch: 'main' })).id);
    assert.equal(run.status, 'succeeded', run.error);
    const sent = traffic.slice(start).filter(t => t.path === '/notify');
    assert.equal(sent.length, 4);
    assert.deepEqual(sent[0].body.allowed_mentions.parse, []);
    assert.equal(sent[2].body.attachments[0].content.type, 'AdaptiveCard');
    assert.ok(!traffic.slice(start).some(t => t.method === 'POST' && /\/issues\/\d+\/comments$/.test(t.path)));
    await api(`/api/notification-providers/${notificationProvider.id}`, 'DELETE', undefined, { status: 409 });
  });
  await check('Cancellation stops a slow model execution', async () => {
    slow = true;
    const queued = await api(`/api/automations/${auto.id}/run`, 'POST', { kind: 'pull_request', repository: 'acme/demo', number: 7 });
    await api(`/api/runs/${queued.id}/cancel`, 'POST', {});
    const run = await waitForRun(queued.id);
    assert.equal(run.status, 'cancelled'); slow = false;
  });
  await mkdir('.qa', { recursive: true });
  await writeFile('.qa/smoke-report.json', JSON.stringify({ checks: report, count: report.length }, null, 2));
  console.log(`\n${report.length} integration checks passed.`);
} finally {
  mock.closeAllConnections();
  await new Promise(resolve => mock.close(resolve));
}
