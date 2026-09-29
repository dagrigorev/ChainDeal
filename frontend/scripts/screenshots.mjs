// Captures the README screenshots by driving Chrome through the real product:
// registers a user on the hosted sign-in page, has the bootstrap admin grant
// roles, links the demo wallets with signature proofs, then visits every page
// (light, dark, mobile). Needs `make web` running and the demo seed loaded.
//
//   make screenshots        (writes docs/screenshots/*.png)
import { chromium } from 'playwright-core';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash, randomBytes } from 'node:crypto';
import { readFileSync, mkdirSync } from 'node:fs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const OUT = ROOT + '/docs/screenshots';
const UI = 'http://localhost:5173';
const API = 'https://chaindeal.localhost';
mkdirSync(OUT, { recursive: true });

const env = Object.fromEntries(readFileSync(ROOT + '/deploy/k8s/.secrets/auth.env', 'utf8').trim().split('\n').map((l) => [l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)]));
const demo = JSON.parse(readFileSync(ROOT + '/scripts/demo-wallets.json', 'utf8'));
const wallets = demo.map((w) => ({ secret: w.secret, pubkey: w.pubkey, address: w.address, label: w.name, kind: w.kind }));
const alice = demo.find((w) => w.name.startsWith('Alice'));
const USER = { name: 'Alice Novak', email: `alice.${Date.now()}@example.test`, password: 'Screens-' + randomBytes(9).toString('base64url') };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const api = async (p) => (await fetch(API + p)).json();
let n = 0;
async function shot(page, name, opts = {}) {
  await sleep(opts.wait ?? 1800);
  const file = `${OUT}/${String(++n).padStart(2, '0')}-${name}.png`;
  if (opts.locator) await page.locator(opts.locator).first().screenshot({ path: file });
  else await page.screenshot({ path: file, fullPage: !!opts.full });
  console.log('  saved', file.replace(ROOT + '/', ''));
}

// --- minimal OAuth client for the admin step (same flow the SPA uses) ---
const b64 = (b) => Buffer.from(b).toString('base64url');
async function adminToken() {
  const jar = new Map();
  const req = async (path, init = {}) => {
    const headers = { ...(init.headers ?? {}), cookie: [...jar].map(([k, v]) => `${k}=${v}`).join('; '), origin: API };
    const r = await fetch(API + path, { ...init, headers, redirect: 'manual' });
    for (const c of r.headers.getSetCookie()) { const kv = c.split(';')[0]; jar.set(kv.slice(0, kv.indexOf('=')), kv.slice(kv.indexOf('=') + 1)); }
    return r;
  };
  const verifier = b64(randomBytes(48));
  const q = new URLSearchParams({ response_type: 'code', client_id: 'chaindeal-web', redirect_uri: API + '/callback', scope: 'openid users:admin',
    state: 's', code_challenge: b64(createHash('sha256').update(verifier).digest()), code_challenge_method: 'S256' });
  const html = await (await req('/oauth/authorize?' + q)).text();
  const f = (k) => html.match(new RegExp(`name="${k}" value="([^"]*)"`))[1];
  const r = await req('/oauth/login', { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ req: f('req'), csrf: f('csrf'), email: env.AUTH_BOOTSTRAP_ADMIN_EMAIL, password: env.AUTH_BOOTSTRAP_ADMIN_PASSWORD }) });
  const code = new URL(r.headers.get('location')).searchParams.get('code');
  const t = await req('/oauth/token', { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code', code, redirect_uri: API + '/callback', client_id: 'chaindeal-web', code_verifier: verifier }) });
  return (await t.json()).access_token;
}

async function signInUI(page, register) {
  await page.getByRole('button', { name: 'Sign in', exact: true }).first().click();
  await page.waitForURL(/\/oauth\/authorize/);
  if (register) {
    await shot(page, 'hosted-sign-in', { wait: 600 });
    await page.getByRole('link', { name: 'Create an account' }).click();
    await page.fill('#name', USER.name);
    await page.fill('#email', USER.email);
    await page.fill('#password', USER.password);
    await page.fill('#password2', USER.password);
    await shot(page, 'hosted-register', { wait: 300 });
    await page.getByRole('button', { name: 'Create account' }).click();
  } else {
    await page.fill('#email', USER.email);
    await page.fill('#password', USER.password);
    await page.getByRole('button', { name: 'Sign in' }).click();
  }
  await page.waitForURL(/localhost:5173\/#/);
  await sleep(2000);
}

const browser = await chromium.launch({ executablePath: process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'light' });
const page = await ctx.newPage();

console.log('Removing previous screenshot accounts…');
{
  const at0 = await adminToken();
  const all = await (await fetch(`${API}/oauth/admin/users?offset=0&limit=200`, { headers: { authorization: `Bearer ${at0}` } })).json();
  for (const u of all.items.filter((u) => !u.roles.includes('admin') || u.name !== 'Administrator')) {
    if (u.name === 'Administrator') continue;
    await fetch(`${API}/oauth/admin/users/${u.id}`, { method: 'DELETE', headers: { authorization: `Bearer ${at0}` } });
    console.log('  deleted', u.name);
  }
}
console.log('Setting up the demo identity…');
await page.goto(UI + '/#/');
await page.evaluate(([ws, a]) => {
  localStorage.setItem('chaindeal.wallets', JSON.stringify(ws));
  localStorage.setItem('chaindeal.active', JSON.stringify(a));
}, [wallets, alice.address]);
await page.goto(UI + '/#/wallet');
await sleep(1500);
await signInUI(page, true);

// Admin grants Alice operator + admin (this revokes her sessions by design).
const at = await adminToken();
const found = await (await fetch(`${API}/oauth/admin/users?email=${encodeURIComponent(USER.email)}`, { headers: { authorization: `Bearer ${at}` } })).json();
await fetch(`${API}/oauth/admin/users/${found.items[0].id}`, { method: 'PATCH', headers: { authorization: `Bearer ${at}`, 'content-type': 'application/json' }, body: JSON.stringify({ roles: ['user', 'operator', 'admin'] }) });
await page.goto(UI + '/#/wallet');
await page.reload();
await sleep(2000);
await signInUI(page, false);

// Link every demo wallet (challenge → WASM signature → verification).
await page.goto(UI + '/#/wallet');
await sleep(2000);
for (let i = 0; i < 8; i++) {
  const link = page.getByRole('button', { name: 'Link', exact: true });
  if ((await link.count()) === 0) break;
  await link.first().click();
  await sleep(1800);
}

console.log('Capturing pages…');
await page.goto(UI + '/#/');
await shot(page, 'desk', { wait: 3500 });
await page.goto(UI + '/#/deals');
await shot(page, 'deals-docket', { wait: 3000 });

const deals = (await api(`/api/accounts/${alice.address}`)).deals;
const byTitle = (t) => deals.find((d) => d.title.startsWith(t))?.id;
const headphones = byTitle('Noise-cancelling');
await page.goto(UI + `/#/deals/${headphones}`);
await shot(page, 'deal-bridge-custody', { wait: 3000 });
const rack = (await api(`/api/deals?limit=50&type=B2B&status=resolved`)).find((d) => d.title.startsWith('Q4 server rack'))?.id
  ?? (await api('/api/deals?limit=1&type=B2B&status=resolved'))[0].id;
await page.goto(UI + `/#/deals/${rack}`);
await shot(page, 'deal-bridge-ruling-split', { wait: 3000 });
const failed = (await api('/api/deals?limit=1&type=B2B&status=failed'))[0].id;
await page.goto(UI + `/#/deals/${failed}`);
await shot(page, 'deal-bridge-failed', { wait: 3000 });

// Proposing a deal: pick Acme, add an item, see the WASM-computed contract preview.
await page.goto(UI + '/#/deals/new');
await sleep(1500);
await page.locator('.picker input').first().click();
await page.locator('.picker input').first().fill('Acme');
await sleep(1200);
await page.locator('.picker-list li[role=option]').filter({ hasText: 'Acme' }).first().click();
await page.getByPlaceholder('What is being traded?').fill('Refurbished laptops for the studio');
await page.getByPlaceholder('Description').first().fill('MacBook Pro 14 (refurbished)');
await page.locator('.items-r input').nth(1).fill('3');
await page.getByPlaceholder('0.00').first().fill('1250.50');
await shot(page, 'new-deal-contract-preview', { wait: 1500 });

await page.goto(UI + '/#/live');
await shot(page, 'live-market', { wait: 9000 });
await shot(page, 'live-market-full', { full: true, wait: 500 });
await shot(page, 'cluster-panel', { locator: 'section.card:has(h3:text-matches("Cluster"))', wait: 500 });

await page.goto(UI + '/#/explorer');
await shot(page, 'ledger-strata', { wait: 3000 });
const h = (await api('/api/stats')).height - 3;
await page.goto(UI + `/#/explorer/block/${h}`);
await sleep(2000);
await page.getByRole('button', { name: /Verify block/ }).click();
await shot(page, 'block-verified-in-browser', { wait: 800 });

await page.goto(UI + '/#/accounts');
await shot(page, 'directory', { wait: 3000 });
await page.goto(UI + '/#/wallet');
await shot(page, 'wallet-linked', { wait: 2500 });
await page.goto(UI + '/#/account');
await shot(page, 'account', { wait: 2500 });
await page.goto(UI + '/#/admin');
await shot(page, 'admin-users-audit', { wait: 3000, full: true });

console.log('Dark and mobile variants…');
const state = await ctx.storageState();
const dark = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark', storageState: state });
const dp = await dark.newPage();
await dp.goto(UI + `/#/deals/${rack}`);
await shot(dp, 'dark-deal-bridge', { wait: 3500 });
await dp.goto(UI + '/#/live');
await shot(dp, 'dark-live-market', { wait: 9000 });

const mobile = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, colorScheme: 'light', storageState: state });
const mp = await mobile.newPage();
await mp.goto(UI + `/#/deals/${headphones}`);
await shot(mp, 'mobile-deal', { wait: 3500 });
await mp.goto(UI + '/#/deals');
await mp.getByRole('tab', { name: 'All network deals' }).click();
await shot(mp, 'mobile-docket', { wait: 2500 });

// Added last so earlier file numbers (referenced by the README) stay stable.
await page.goto(UI + '/#/live');
await shot(page, 'grpc-transaction-stream', { locator: 'section.card:has(h3:text-matches("Transaction stream"))', wait: 9000 });

await browser.close();
console.log(`Done: ${n} screenshots in docs/screenshots`);
