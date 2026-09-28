// Seeds a running ChainDeal node with demo accounts and deals covering every
// deal type and lifecycle state. Signs with the same Rust/WASM wallet the
// browser uses. Doubles as an end-to-end test: exits non-zero on any surprise.
//
//   node scripts/seed.mjs [http://localhost:8080]
//
// Writes scripts/demo-wallets.json — import any of those secrets in the UI.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import * as w from '../frontend/src/wasm/chaindeal_wallet.js';

const here = dirname(fileURLToPath(import.meta.url));
w.initSync({ module: readFileSync(join(here, '../frontend/src/wasm/chaindeal_wallet_bg.wasm')) });

const BASE = process.argv[2] || process.env.CHAINDEAL_API || 'https://chaindeal.localhost';
const API = BASE + '/api';

// Machine-to-machine OAuth: the seed service authenticates with client
// credentials (client_credentials grant, HTTP Basic) and gets a short-lived
// token allowed to submit transactions for its demo wallets.
const SEED_SECRET = process.env.CHAINDEAL_SEED_SECRET;
if (!SEED_SECRET) {
  console.error('Set CHAINDEAL_SEED_SECRET (make seed does this from deploy/k8s/.secrets).');
  process.exit(1);
}
let token = null;
let tokenExp = 0;
async function accessToken() {
  if (token && Date.now() < tokenExp - 30_000) return token;
  const res = await fetch(BASE + '/oauth/token', {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      authorization: 'Basic ' + Buffer.from(`chaindeal-seed:${SEED_SECRET}`).toString('base64'),
    },
    body: new URLSearchParams({ grant_type: 'client_credentials', scope: 'deals:write tx:any' }),
  });
  const body = await res.json();
  if (!res.ok) throw new Error(`token request failed: ${body.error_description ?? body.error}`);
  token = body.access_token;
  tokenExp = Date.now() + body.expires_in * 1000;
  return token;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const DEAL = (x) => Math.round(x * 100);

async function api(path, init) {
  const res = await fetch(API + path, init);
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw Object.assign(new Error(body.error || res.statusText), { status: res.status });
  return body;
}

async function waitFor(hash) {
  for (let i = 0; i < 60; i++) {
    const tx = await api(`/tx/${hash}`).catch(() => null);
    if (tx && tx.status !== 'pending') return tx;
    await sleep(500);
  }
  throw new Error(`tx ${hash} not confirmed in time`);
}

async function send(who, action) {
  const signed = w.signAction(who.secret, JSON.stringify(action), Date.now());
  const { hash } = await api('/tx', { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${await accessToken()}` }, body: signed });
  const tx = await waitFor(hash);
  if (tx.status !== 'confirmed') throw new Error(`${who.name} ${action.type} rejected: ${tx.error}`);
  console.log(`  ✓ ${who.name.padEnd(16)} ${action.type.padEnd(16)} block #${tx.block_height}${tx.deal_id ? '  ' + tx.deal_id : ''}`);
  return tx;
}

async function expectRefused(who, action, fragment) {
  const signed = w.signAction(who.secret, JSON.stringify(action), Date.now());
  try {
    await api('/tx', { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${await accessToken()}` }, body: signed });
  } catch (e) {
    if (e.status === 422 && e.message.includes(fragment)) {
      console.log(`  ✓ ${who.name.padEnd(16)} ${action.type.padEnd(16)} refused as expected: "${e.message}"`);
      return;
    }
    throw new Error(`unexpected error for ${action.type}: ${e.message}`);
  }
  throw new Error(`${action.type} should have been refused`);
}

function wallet(name, kind) {
  return { name, kind, ...JSON.parse(w.generateWallet()) };
}

const people = {
  alice: wallet('Alice Novak', 'consumer'),
  bob: wallet('Bob Reyes', 'consumer'),
  acme: wallet('Acme Electronics', 'business'),
  globex: wallet('Globex Logistics', 'business'),
  court: wallet('TrustCourt Arbitration', 'business'),
};
const { alice, bob, acme, globex, court } = people;

console.log(`Seeding ${API}\n\n1. Registering accounts (each receives 10,000.00 DEAL)`);
await Promise.all(Object.values(people).map((p) => send(p, { type: 'register', name: p.name, kind: p.kind })));

const item = (name, qty, price) => ({ name, qty, unit_price: DEAL(price) });
const create = (who, role, counterparty, title, description, items, arbiter = null) =>
  send(who, { type: 'create_deal', role, counterparty: counterparty.address, title, description, items, arbiter: arbiter?.address ?? null });

console.log('\n2. C2C — second-hand bike, no arbiter, completed with rating');
const bike = (await create(alice, 'seller', bob, 'Vintage road bike', 'Steel frame, 56cm, new tyres.', [item('Road bike', 1, 450)])).deal_id;
await send(bob, { type: 'accept_deal', deal_id: bike });
await send(bob, { type: 'fund_deal', deal_id: bike });
await expectRefused(bob, { type: 'cancel_deal', deal_id: bike, reason: 'nah' }, 'cannot withdraw');
await send(alice, { type: 'mark_shipped', deal_id: bike, tracking: 'Local pickup' });
await send(bob, { type: 'confirm_receipt', deal_id: bike, rating: 5 });

console.log('\n3. B2C — headphones with arbiter, shipped and awaiting the consumer');
const phones = (await create(acme, 'seller', alice, 'Noise-cancelling headphones', 'Includes 2-year extended warranty.',
  [item('ANC headphones', 1, 299.99), item('Extended warranty', 1, 49)], court)).deal_id;
await send(alice, { type: 'accept_deal', deal_id: phones });
await send(alice, { type: 'fund_deal', deal_id: phones });
await send(acme, { type: 'mark_shipped', deal_id: phones, tracking: 'DHL 4829-1177-02' });

console.log('\n4. B2C — consumer withdraws a funded order before shipment');
const watch = (await create(acme, 'seller', bob, 'Smartwatch', '', [item('Smartwatch S3', 1, 189)], court)).deal_id;
await send(bob, { type: 'accept_deal', deal_id: watch });
await send(bob, { type: 'fund_deal', deal_id: watch });
await send(bob, { type: 'cancel_deal', deal_id: watch, reason: 'Found it cheaper locally' });

console.log('\n5. B2B — buyer-initiated rack order, bond locked, disputed and resolved 40/60');
const rack = (await create(globex, 'buyer', acme, 'Q4 server rack order', 'Delivery to Rotterdam DC, installation included.',
  [item('42U server rack', 4, 1900), item('On-site installation', 1, 600)], court)).deal_id;
await send(acme, { type: 'accept_deal', deal_id: rack });
await send(globex, { type: 'fund_deal', deal_id: rack });
await send(acme, { type: 'mark_shipped', deal_id: rack, tracking: 'FREIGHT-NL-99812' });
await send(globex, { type: 'open_dispute', deal_id: rack, reason: 'Two of four racks arrived with bent rails.' });
await expectRefused(globex, { type: 'resolve_dispute', deal_id: rack, buyer_refund_bps: 10000, note: '' }, 'arbiter');
await send(court, { type: 'resolve_dispute', deal_id: rack, buyer_refund_bps: 4000, note: 'Damage documented on delivery photos.' });

console.log('\n6. B2B — logistics contract funded, in progress');
const freight = (await create(globex, 'seller', acme, 'Monthly freight contract — October', '',
  [item('Container slot (40ft)', 6, 350), item('Customs handling', 1, 220)], court)).deal_id;
await send(acme, { type: 'accept_deal', deal_id: freight });
await send(acme, { type: 'fund_deal', deal_id: freight });

console.log('\n7. C2B — freelance design offer awaiting the business');
await create(bob, 'seller', globex, 'Logo redesign', 'Three concepts, two revision rounds, SVG + PNG delivery.', [item('Logo design package', 1, 600)]);

console.log('\n8. Plain transfer');
await send(alice, { type: 'transfer', to: bob.address, amount: DEAL(25), memo: 'Thanks for the coffee' });

// Verify the blocks this seed just wrote (the full chain may hold 1M+ txs;
// the Ledger page walks all of it in chunks).
const { height } = await api('/stats');
const report = await api(`/chain/verify?from=${Math.max(0, height - 60)}&count=61`);
console.log(`\nVerified recent blocks: ${report.valid ? 'VALID' : 'INVALID'} — ${report.blocks_checked} blocks, ${report.txs_checked} txs in ${report.elapsed_ms}ms`);
if (!report.valid) {
  console.error(report.errors);
  process.exit(1);
}

const wallets = JSON.stringify(Object.values(people), null, 2);
const out = join(here, 'demo-wallets.json');
writeFileSync(out, wallets);
// Served by Vite so the Wallet page can offer one-click import (dev only; gitignored).
writeFileSync(join(here, '../frontend/public/demo-wallets.json'), wallets);
console.log(`\nWallets written to ${out} — or use "Import demo wallets" on the Wallet page.`);
