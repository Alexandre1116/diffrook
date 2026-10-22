// Owner tool. Never distribute the private signing key with an installation.
import { createPrivateKey, createPublicKey, generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { access, mkdir, readFile, stat, writeFile } from 'node:fs/promises';
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
  } else if (command === 'issue' || command === 'quote') {
    const options = {};
    const allowed = command === 'quote' ? ['request'] : ['request', 'installation', 'customer', 'out', 'expires', 'plan', 'period', 'users', 'automation-packs'];
    for (let i = 0; i < args.length; i += 2) {
      const name = args[i]?.replace(/^--/, '');
      if (!args[i]?.startsWith('--') || !allowed.includes(name) || options[name] !== undefined || !args[i + 1]) throw new Error('Invalid or repeated option');
      options[name] = args[i + 1];
    }
    let request;
    if (options.request) {
      if (['installation', 'plan', 'period', 'users', 'automation-packs'].some(name => options[name] !== undefined)) throw new Error('--request cannot be combined with plan, installation or allowance overrides');
      const path = resolve(options.request);
      if ((await stat(path)).size > 32768) throw new Error('License request is too large');
      request = JSON.parse(await readFile(path, 'utf8'));
      const fields = ['request_version', 'hosting', 'installation_id', 'plan', 'billing_period', 'users', 'extra_automation_packs', 'automations', 'currency', 'tax_included', 'quoted_amount'];
      if (!request || Array.isArray(request) || typeof request !== 'object' || Object.keys(request).some(name => !fields.includes(name)) || request.request_version !== 1 || request.hosting !== 'self_hosted') throw new Error('Unsupported self-hosted license request');
      if (typeof request.installation_id !== 'string' || typeof request.plan !== 'string' || typeof request.billing_period !== 'string' || !Number.isSafeInteger(request.users) || !Number.isSafeInteger(request.extra_automation_packs)) throw new Error('Invalid license request fields');
      Object.assign(options, {installation:request.installation_id, plan:request.plan, period:request.billing_period});
      if (['teams', 'enterprise'].includes(request.plan)) Object.assign(options, {users:String(request.users)});
      if (request.plan === 'enterprise') Object.assign(options, {'automation-packs':String(request.extra_automation_packs)});
    } else if (command === 'quote') throw new Error('quote requires --request FILE');
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(options.installation || '')) throw new Error('--installation must be the instance UUID from Settings');
    if (command === 'issue' && (!options.customer?.trim() || Buffer.byteLength(options.customer.trim()) > 512 || !options.out)) throw new Error('--customer and --out are required; customer must be at most 512 UTF-8 bytes');
    const integer = (name, fallback, minimum = 1, maximum = 2147483647) => {
      if (!options[name]) return fallback;
      const n = Number(options[name]);
      if (!Number.isInteger(n) || n < minimum || n > maximum) throw new Error(`--${name} must be an integer from ${minimum} to ${maximum}`);
      return n;
    };
    const catalog = JSON.parse(await readFile(resolve(root, 'src/plans.json'), 'utf8'));
    const plan = catalog.self_hosted.find(p => p.id === options.plan && p.commercial_use);
    if (!plan) throw new Error('--plan must be freelancer, teams or enterprise; Cloud is coming soon');
    if (!['monthly', 'annual'].includes(options.period)) throw new Error('--period must be monthly or annual');
    if (!plan.per_user && options.users !== undefined) throw new Error('Only Teams and Enterprise accept --users; Freelancer has a fixed allowance');
    if (plan.id !== 'enterprise' && options['automation-packs'] !== undefined) throw new Error('Only Enterprise accepts --automation-packs');
    // Teams is priced per user up to its cap; Enterprise starts at its minimum.
    const minimum = plan.min_users ?? 1;
    const users = plan.per_user ? integer('users', minimum, minimum, plan.users ?? 2147483647) : plan.users;
    const packs = integer('automation-packs', 0, 0);
    const automations = plan.per_user ? users * plan.automations_per_user + packs * catalog.self_hosted_automation_pack.automations : plan.automations;
    if (!Number.isSafeInteger(automations) || automations > 2147483647) throw new Error('Automation allowance is too large');
    if (request && (request.users !== users || request.extra_automation_packs !== packs || request.automations !== automations)) throw new Error('Request allowances do not match the plan catalog');
    // Enterprise and its automation packs are priced by arrangement (null in the catalog).
    const unit = plan[`${options.period}_eur`], packUnit = catalog.self_hosted_automation_pack[`${options.period}_eur`];
    const price = unit == null || (packs > 0 && packUnit == null) ? null
      : (Math.round(unit * 100) * (plan.per_user ? users : 1) + Math.round((packUnit ?? 0) * 100) * packs) / 100;
    if (request && (request.currency !== catalog.currency || request.tax_included !== false || request.quoted_amount !== price)) console.error('Submitted quote differs from the current catalog. Confirm the current price with the customer before collecting payment or issuing a license.');
    if (command === 'quote') {
      console.log(JSON.stringify({installation_id:options.installation.toLowerCase(), plan:plan.id, billing_period:options.period, users, automations, extra_automation_packs:packs, currency:catalog.currency, tax_included:false, amount:price, payment_verified:false}, null, 2));
    } else {
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
    console.log(`${plan.name} license written to ${output}: ${users} user(s), ${automations} automations, ${price == null ? 'price by arrangement' : `${price} EUR/${options.period === 'annual' ? 'year' : 'month'} before tax`}. This tool does not collect payment.`);
    console.log('Install it through DIFFROOK_LICENSE_FILE and restart.');
    }
  } else {
    throw new Error('Usage: node scripts/license.mjs init | quote --request FILE | issue --request FILE --customer NAME --out FILE [--expires ISO_DATE] | issue --plan freelancer|teams|enterprise --period monthly|annual --installation UUID --customer NAME --out FILE [--expires ISO_DATE] [--users N (Teams 1-10, Enterprise 10+)] [--automation-packs N (Enterprise only)]');
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
