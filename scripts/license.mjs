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
    const allowed = ['installation', 'customer', 'out', 'expires', 'plan', 'period', 'users', 'automation-packs'];
    for (let i = 0; i < args.length; i += 2) {
      const name = args[i]?.replace(/^--/, '');
      if (!args[i]?.startsWith('--') || !allowed.includes(name) || options[name] !== undefined || !args[i + 1]) throw new Error('Invalid or repeated option');
      options[name] = args[i + 1];
    }
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(options.installation || '')) throw new Error('--installation must be the instance UUID from Settings');
    if (!options.customer?.trim() || Buffer.byteLength(options.customer.trim()) > 512 || !options.out) throw new Error('--customer and --out are required; customer must be at most 512 UTF-8 bytes');
    const integer = (name, fallback, minimum = 1) => {
      if (!options[name]) return fallback;
      const n = Number(options[name]);
      if (!Number.isInteger(n) || n < minimum || n > 2147483647) throw new Error(`--${name} must be an integer of at least ${minimum}`);
      return n;
    };
    const catalog = JSON.parse(await readFile(resolve(root, 'src/plans.json'), 'utf8'));
    const plan = catalog.self_hosted.find(p => p.id === options.plan && p.commercial_use);
    if (!plan) throw new Error('--plan must be freelancer, teams or enterprise; Cloud is coming soon');
    if (!['monthly', 'annual'].includes(options.period)) throw new Error('--period must be monthly or annual');
    if (plan.id !== 'enterprise' && (options.users !== undefined || options['automation-packs'] !== undefined)) throw new Error('Only Enterprise accepts --users and --automation-packs; other plans have fixed allowances');
    const users = plan.per_user ? integer('users', plan.min_users, plan.min_users) : plan.users;
    const packs = integer('automation-packs', 0, 0);
    const automations = plan.per_user ? users * plan.automations_per_user + packs * catalog.self_hosted_automation_pack.automations : plan.automations;
    if (!Number.isSafeInteger(automations) || automations > 2147483647) throw new Error('Automation allowance is too large');
    const issued = Math.floor(Date.now() / 1000);
    // Clamp the day when renewing from the end of a month or a leap day.
    const expiryDate = new Date(issued * 1000);
    const day = expiryDate.getUTCDate();
    expiryDate.setUTCDate(1);
    expiryDate.setUTCMonth(expiryDate.getUTCMonth() + (options.period === 'annual' ? 12 : 1));
    const lastDay = new Date(Date.UTC(expiryDate.getUTCFullYear(), expiryDate.getUTCMonth() + 1, 0)).getUTCDate();
    expiryDate.setUTCDate(Math.min(day, lastDay));
    const expiry = options.expires ? Math.floor(Date.parse(options.expires) / 1000) : Math.floor(expiryDate.getTime() / 1000);
    if (!Number.isSafeInteger(expiry) || expiry <= issued) throw new Error('--expires must be a future ISO date/time');
    const key = createPrivateKey(await readFile(privatePath));
    if (key.asymmetricKeyType !== 'ed25519' || publicHex(key) !== (await readFile(publicPath, 'utf8')).trim()) throw new Error('Signing key does not match the public key compiled into Diffrook');
    const payload = Buffer.from(JSON.stringify({ version: 2, product: 'diffrook', edition: plan.id, billing_period: options.period, extra_automation_packs: packs, license_id: randomUUID(), installation_id: options.installation.toLowerCase(), customer: options.customer.trim(), issued_at: issued, expires_at: expiry, max_users: users, max_automations: automations }));
    const signature = sign(null, Buffer.concat([Buffer.from('diffrook-license-v1\0'), payload]), key);
    const output = resolve(options.out);
    await mkdir(dirname(output), { recursive: true });
    await writeFile(output, JSON.stringify({ payload: payload.toString('base64url'), signature: signature.toString('base64url') }) + '\n', { flag: 'wx', mode: 0o600 });
    const price = plan[`${options.period}_eur`] * (plan.per_user ? users : 1) + packs * catalog.self_hosted_automation_pack[`${options.period}_eur`];
    console.log(`${plan.name} license written to ${output}: ${users} user(s), ${automations} automations, ${price} EUR/${options.period === 'annual' ? 'year' : 'month'} before tax. This tool does not collect payment.`);
    console.log('Install it through DIFFROOK_LICENSE_FILE and restart.');
  } else {
    throw new Error('Usage: node scripts/license.mjs init | issue --plan freelancer|teams|enterprise --period monthly|annual --installation UUID --customer NAME --out FILE [--expires ISO_DATE] [--users N --automation-packs N (Enterprise only)]');
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
