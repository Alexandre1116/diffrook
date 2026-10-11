// Only for the isolated release-smoke instance and its disposable test credentials.
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
const [mode, path] = process.argv.slice(2);
assert.ok(['snapshot', 'verify'].includes(mode) && path);
const origin = process.env.DIFFROOK_TEST_URL;
assert.ok(origin && new URL(origin).hostname === '127.0.0.1', 'Use an isolated loopback instance');
let cookie = '';
async function api(endpoint, body) {
  const response = await fetch(origin + endpoint, {
    method: body ? 'POST' : 'GET',
    headers: { 'content-type': 'application/json', 'x-diffrook-request': '1', cookie },
    body: body ? JSON.stringify(body) : undefined,
  });
  assert.ok(response.ok, `${endpoint}: HTTP ${response.status}`);
  const session = response.headers.get('set-cookie');
  if (session) cookie = session.split(';')[0];
  return response.json();
}
assert.equal((await api('/api/status')).setup_required, false);
await api('/api/login', { username: 'tester', password: 'Diffrook-test-password-123' });
const license = await api('/api/license');
const snapshot = { installation_id: license.installation_id, edition: license.edition, limits: license.limits };
for (const resource of ['connections', 'providers', 'automations', 'runs', 'notification-providers']) {
  snapshot[resource] = (await api(`/api/${resource}`)).sort((a, b) => a.id.localeCompare(b.id));
  assert.ok(snapshot[resource].length > 0, `${resource} must be populated`);
}
if (mode === 'snapshot') await writeFile(path, JSON.stringify(snapshot));
else assert.deepEqual(snapshot, JSON.parse(await readFile(path, 'utf8')));
console.log(`PASS ${mode}: login, installation identity, allowances and persisted resources`);
