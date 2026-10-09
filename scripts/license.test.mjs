import test from 'node:test';
import assert from 'node:assert/strict';
import { createPublicKey, verify } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';

test('owner CLI protects keys and issues verifiable installation-bound licenses', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'diffrook-license-tool-'));
  t.after(async () => {
    const target = resolve(directory);
    assert.equal(dirname(target), resolve(tmpdir()));
    assert.ok(basename(target).startsWith('diffrook-license-tool-'));
    await rm(target, { recursive: true, force: true });
  });
  await mkdir(join(directory, 'scripts'));
  await mkdir(join(directory, 'src'));
  await copyFile(new URL('./license.mjs', import.meta.url), join(directory, 'scripts/license.mjs'));
  await copyFile(new URL('../src/plans.json', import.meta.url), join(directory, 'src/plans.json'));
  const run = (...args) => spawnSync(process.execPath, [join(directory, 'scripts/license.mjs'), ...args], { encoding: 'utf8', cwd: directory });
  assert.equal(run('init').status, 0);
  const originalKey = await readFile(join(directory, 'secrets/license-signing-key.pem'));
  assert.notEqual(run('init').status, 0);
  assert.deepEqual(await readFile(join(directory, 'secrets/license-signing-key.pem')), originalKey);
  const id = '12345678-1234-4321-9876-123456789abc';
  const output = join(directory, 'customer.json');
  const argumentsList = ['issue', '--plan', 'enterprise', '--period', 'annual', '--installation', id, '--customer', 'Test customer', '--users', '20', '--automation-packs', '2', '--out', output];
  assert.equal(run(...argumentsList).status, 0);
  const envelope = JSON.parse(await readFile(output, 'utf8'));
  const payload = Buffer.from(envelope.payload, 'base64url');
  const claims = JSON.parse(payload);
  assert.equal(claims.installation_id, id);
  assert.equal(claims.max_users, 20);
  assert.equal(claims.max_automations, 220);
  assert.equal(claims.version, 2);
  assert.equal(claims.edition, 'enterprise');
  assert.equal(claims.billing_period, 'annual');
  assert.equal(claims.extra_automation_packs, 2);
  assert.ok(claims.expires_at > claims.issued_at + 360 * 86400);
  assert.ok(claims.expires_at <= claims.issued_at + 366 * 86400);
  assert.equal(claims.product, 'diffrook');
  const publicBytes = Buffer.from((await readFile(join(directory, 'src/license-public-key.hex'), 'utf8')).trim(), 'hex');
  const key = createPublicKey({ format: 'jwk', key: { kty: 'OKP', crv: 'Ed25519', x: publicBytes.toString('base64url') } });
  assert.ok(verify(null, Buffer.concat([Buffer.from('diffrook-license-v1\0'), payload]), key, Buffer.from(envelope.signature, 'base64url')));
  const saved = await readFile(output);
  assert.notEqual(run(...argumentsList).status, 0);
  assert.deepEqual(await readFile(output), saved);
  const base = ['issue', '--installation', id, '--customer', 'Test', '--period', 'monthly'];
  for (const [edition, users, automations, price] of [['freelancer', 1, 10, 9], ['teams', 5, 20, 29], ['enterprise', 10, 100, 80]]) {
    const path = join(directory, `${edition}.json`);
    const result = run(...base, '--plan', edition, '--out', path);
    assert.equal(result.status, 0, result.stderr);
    assert.ok(result.stdout.includes(`${price} EUR/month`));
    const issued = JSON.parse(Buffer.from(JSON.parse(await readFile(path, 'utf8')).payload, 'base64url'));
    assert.equal(issued.max_users, users);
    assert.equal(issued.max_automations, automations);
    assert.ok(issued.expires_at > issued.issued_at + 27 * 86400);
    assert.ok(issued.expires_at <= issued.issued_at + 31 * 86400);
  }
  for (const options of [
    ['--plan', 'individual'], ['--plan', 'cloud'], ['--plan', 'business'],
    ['--plan', 'freelancer', '--users', '2'], ['--plan', 'teams', '--automation-packs', '1'],
    ['--plan', 'enterprise', '--users', '9'], ['--plan', 'enterprise', '--automation-packs', '-1'],
    ['--plan', 'enterprise', '--users', '2147483647'], ['--plan', 'teams', '--expires', '2000-01-01'],
  ]) assert.notEqual(run(...base, ...options, '--out', join(directory, 'invalid.json')).status, 0, options.join(' '));
  assert.notEqual(run('issue', '--plan', 'freelancer', '--installation', id, '--customer', 'Test', '--out', join(directory, 'no-period.json')).status, 0);
  await writeFile(join(directory, 'src/license-public-key.hex'), '00'.repeat(32));
  assert.notEqual(run(...base, '--plan', 'freelancer', '--out', join(directory, 'wrong-key.json')).status, 0);
});
