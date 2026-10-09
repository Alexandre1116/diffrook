// Owner tool. Never distribute the private signing key with an installation.
import { createPrivateKey, createPublicKey, generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const privatePath = resolve(root, 'secrets/license-signing-key.pem');
const publicPath = resolve(root, 'src/license-public-key.hex');
const publicHex = key => Buffer.from(createPublicKey(key).export({ format: 'jwk' }).x, 'base64url').toString('hex');
const [command, ...args] = process.argv.slice(2);

try {
  if (command === 'init' && !args.length) {
    for (const path of [privatePath, publicPath]) {
      let exists = true;
      try { await access(path); } catch (error) { if (error.code === 'ENOENT') exists = false; else throw error; }
      if (exists) throw new Error('Issuer files already exist. Initialization requires both key files to be absent.');
    }
    await mkdir(dirname(privatePath), { recursive: true, mode: 0o700 });
    const { privateKey } = generateKeyPairSync('ed25519');
    // Exclusive creation protects an existing issuer key from accidental replacement.
    await writeFile(privatePath, privateKey.export({ type: 'pkcs8', format: 'pem' }), { flag: 'wx', mode: 0o600 });
    await writeFile(publicPath, publicHex(privateKey) + '\n', { flag: 'wx' });
    console.log('Issuer key created in secrets/license-signing-key.pem. Back it up privately.');
    console.log('Commit only src/license-public-key.hex, then build the application.');
  } else if (command === 'issue') {
    const options = {};
    const allowed = ['installation', 'customer', 'out', 'expires', 'users', 'automations'];
    for (let i = 0; i < args.length; i += 2) {
      const name = args[i]?.replace(/^--/, '');
      if (!args[i]?.startsWith('--') || !allowed.includes(name) || options[name] !== undefined || !args[i + 1]) throw new Error('Invalid or repeated option');
      options[name] = args[i + 1];
    }
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(options.installation || '')) throw new Error('--installation must be the instance UUID from Settings');
    if (!options.customer?.trim() || options.customer.length > 512 || !options.out) throw new Error('--customer and --out are required');
    const limit = name => {
      if (!options[name]) return null;
      const n = Number(options[name]);
      if (!Number.isInteger(n) || n < 1 || n > 2147483647) throw new Error(`--${name} must be a positive integer`);
      return n;
    };
    const issued = Math.floor(Date.now() / 1000);
    const expiry = options.expires ? Math.floor(Date.parse(options.expires) / 1000) : null;
    if (expiry !== null && (!Number.isSafeInteger(expiry) || expiry <= issued)) throw new Error('--expires must be a future ISO date/time');
    const key = createPrivateKey(await readFile(privatePath));
    if (key.asymmetricKeyType !== 'ed25519' || publicHex(key) !== (await readFile(publicPath, 'utf8')).trim()) throw new Error('Signing key does not match the public key compiled into Diffrook');
    const payload = Buffer.from(JSON.stringify({ version: 1, product: 'diffrook', edition: 'business', license_id: randomUUID(), installation_id: options.installation.toLowerCase(), customer: options.customer.trim(), issued_at: issued, expires_at: expiry, max_users: limit('users'), max_automations: limit('automations') }));
    const signature = sign(null, Buffer.concat([Buffer.from('diffrook-license-v1\0'), payload]), key);
    const output = resolve(options.out);
    await mkdir(dirname(output), { recursive: true });
    await writeFile(output, JSON.stringify({ payload: payload.toString('base64url'), signature: signature.toString('base64url') }) + '\n', { flag: 'wx', mode: 0o600 });
    console.log(`Business license written to ${output}. Install it through DIFFROOK_LICENSE_FILE and restart.`);
  } else {
    throw new Error('Usage: node scripts/license.mjs init | issue --installation UUID --customer NAME --out FILE [--expires ISO_DATE] [--users N] [--automations N]');
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
