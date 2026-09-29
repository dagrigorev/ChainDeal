// End-to-end checks for the gRPC LedgerService, run against the live cluster
// through the TLS ingress, over both transports:
//   native gRPC (HTTP/2) — what services use;
//   gRPC-Web            — what browsers use.
//
//   make grpc-test      (sets NODE_EXTRA_CA_CERTS; needs `make k8s-up`)
// Exits non-zero on the first failed check.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Code, ConnectError, createClient } from '@connectrpc/connect';
import { createGrpcTransport, createGrpcWebTransport } from '@connectrpc/connect-node';
import { LedgerService, TxReceipt_Outcome as Outcome } from '../src/gen/chaindeal/v1/ledger_pb.js';
import * as w from '../src/wasm/chaindeal_wallet.js';

const here = dirname(fileURLToPath(import.meta.url));
w.initSync({ module: readFileSync(join(here, '../src/wasm/chaindeal_wallet_bg.wasm')) });
const BASE = process.env.CHAINDEAL_URL || 'https://chaindeal.localhost';
const secrets = Object.fromEntries(
  readFileSync(join(here, '../../deploy/k8s/.secrets/auth.env'), 'utf8').trim().split('\n').map((l) => [l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)]),
);

let passed = 0;
function check(cond, msg) {
  if (!cond) {
    console.error(`  ✗ ${msg}`);
    process.exit(1);
  }
  passed++;
  console.log(`  ✓ ${msg}`);
}
const sha256 = (s) => createHash('sha256').update(s).digest('hex');

/** Collects stream items for `ms` (or until `max`), then cancels the stream. */
async function take(stream, ms, max = Infinity) {
  const out = [];
  try {
    for await (const x of stream) {
      out.push(x);
      if (out.length >= max) break;
    }
  } catch (e) {
    if (ConnectError.from(e).code !== Code.Canceled) throw e;
  }
  return out;
}
const timed = (ms) => AbortSignal.timeout(ms);

async function token(client, secret, scope) {
  const r = await fetch(`${BASE}/oauth/token`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded', authorization: 'Basic ' + Buffer.from(`${client}:${secret}`).toString('base64') },
    body: new URLSearchParams({ grant_type: 'client_credentials', scope }),
  });
  return { status: r.status, body: await r.json() };
}

/** Sends requests on a SubmitTxs stream and returns the receipts (or the error). */
async function submit(client, requests, bearer) {
  const headers = bearer ? { authorization: `Bearer ${bearer}` } : {};
  try {
    const receipts = [];
    for await (const r of client.submitTxs((async function* () { yield* requests; })(), { headers, signal: timed(10_000) })) receipts.push(r);
    return { receipts };
  } catch (e) {
    return { error: ConnectError.from(e) };
  }
}

const signed = (action) => {
  const kp = JSON.parse(w.generateWallet());
  const tx = JSON.parse(w.signAction(kp.secret, JSON.stringify(action), Date.now()));
  return { hash: tx.hash, bodyJson: JSON.stringify(tx.body), signature: tx.signature };
};

// gRPC-Web runs over HTTP/2 here, as browsers negotiate with the ingress over
// TLS. (Over HTTP/1.1 only server streams are dependable through a proxy —
// checked separately below; browsers never send client streams anyway.)
const transports = {
  'gRPC (HTTP/2)': createGrpcTransport({ baseUrl: BASE }),
  'gRPC-Web': createGrpcWebTransport({ baseUrl: BASE, httpVersion: '2' }),
};

for (const [name, transport] of Object.entries(transports)) {
  const ledger = createClient(LedgerService, transport);
  console.log(`\n${name}: Watch`);
  const events = await take(ledger.watch({ withTransactions: true }, { signal: timed(6000) }), 6000);
  check(events.length > 10, `live events arrive through the ingress (${events.length} in 6 s)`);
  const seqs = events.map((e) => e.seq);
  check(seqs.every((s, i) => i === 0 || s > seqs[i - 1]), 'sequence numbers strictly increase');
  const kinds = new Set(events.map((e) => e.event.case));
  check(kinds.has('pending') && kinds.has('block'), `admissions and sealed blocks both stream (${[...kinds].join(', ')})`);

  const blocks = events.filter((e) => e.event.case === 'block').map((e) => e.event.value);
  const txs = blocks.flatMap((b) => b.transactions);
  check(blocks.every((b) => b.transactions.length === b.txCount), 'with_transactions: every block carries all its transactions');
  check(txs.length > 0 && txs.every((t) => sha256(t.bodyJson) === t.hash), `every streamed transaction hashes to its id (${txs.length} checked)`);
  check(txs.every((t) => w.verifyTx(JSON.stringify({ hash: t.hash, body: JSON.parse(t.bodyJson), signature: t.signature })) === ''), 'every streamed signature verifies (ed25519, Rust/WASM)');
  check(txs.every((t) => t.status === 'confirmed' && blocks.some((b) => b.height === t.blockHeight)), 'streamed transactions are confirmed in the block that carries them');

  const last = seqs[seqs.length - 1];
  const resumed = await take(ledger.watch({ afterSeq: last - 30n }, { signal: timed(4000) }), 4000, 30);
  check(resumed.length === 30 && resumed.every((e, i) => e.seq === last - 29n + BigInt(i)), 'resume after a sequence number replays the gap exactly, without loss or duplicates');

  console.log(`${name}: FollowBlocks`);
  const tip = BigInt(blocks[blocks.length - 1]?.height ?? 0n);
  const followed = await take(ledger.followBlocks({ fromHeight: tip - 5n, toHeight: tip }, { signal: timed(15_000) }), 15_000);
  check(followed.length === 6 && followed.every((b, i) => b.header.height === tip - 5n + BigInt(i)), 'catch-up returns the exact height range, in order');
  check(followed.every((b, i) => i === 0 || b.header.prevHash === followed[i - 1].hash), 'blocks chain by prev_hash');
  check(followed.every((b) => b.transactions.length === b.header.txCount), 'each block carries its full transaction list');
  const live = await take(ledger.followBlocks({ fromHeight: tip + 1n }, { signal: timed(10_000) }), 10_000, 1);
  check(live.length === 1 && live[0].header.height === tip + 1n, 'then follows live: the next sealed block arrives');

  console.log(`${name}: SubmitTxs authorization`);
  const reg = signed({ type: 'register', name: 'gRPC e2e', kind: 'consumer' });
  let r = await submit(ledger, [{ ref: 1n, ...reg }]);
  check(r.error?.code === Code.Unauthenticated, 'no token → UNAUTHENTICATED');
  r = await submit(ledger, [{ ref: 1n, ...reg }], 'not.a.jwt');
  check(r.error?.code === Code.Unauthenticated, 'forged token → UNAUTHENTICATED');
  const noWrite = await token('chaindeal-seed', secrets.AUTH_SEED_CLIENT_SECRET, 'tx:any');
  r = await submit(ledger, [{ ref: 1n, ...reg }], noWrite.body.access_token);
  check(r.error?.code === Code.PermissionDenied, 'token without deals:write → PERMISSION_DENIED');
  const writeOnly = await token('chaindeal-seed', secrets.AUTH_SEED_CLIENT_SECRET, 'deals:write');
  r = await submit(ledger, [{ ref: 7n, ...reg }], writeOnly.body.access_token);
  check(r.receipts?.[0]?.outcome === Outcome.FORBIDDEN && r.receipts[0].ref === 7n, 'unlinked wallet without tx:any → FORBIDDEN receipt (matched by ref)');

  console.log(`${name}: SubmitTxs receipts`);
  const svc = (await token('chaindeal-seed', secrets.AUTH_SEED_CLIENT_SECRET, 'deals:write tx:any')).body.access_token;
  const good = signed({ type: 'register', name: `gRPC e2e ${name}`, kind: 'consumer' });
  const tampered = { ...signed({ type: 'register', name: 'x', kind: 'consumer' }), signature: '00'.repeat(64) };
  const poor = signed({ type: 'transfer', to: JSON.parse(w.generateWallet()).address, amount: 1_000_00, memo: 'no funds' });
  r = await submit(ledger, [
    { ref: 1n, ...good },
    { ref: 2n, ...good },
    { ref: 3n, ...tampered },
    { ref: 4n, ...poor },
    { ref: 5n, hash: 'ab', bodyJson: '{not json', signature: '' },
  ], svc);
  const by = Object.fromEntries((r.receipts ?? []).map((x) => [Number(x.ref), x]));
  check(r.receipts?.length === 5, 'one receipt per streamed transaction');
  check(by[1]?.outcome === Outcome.ADMITTED, 'valid transaction → ADMITTED');
  check([Outcome.INVALID, Outcome.REJECTED].includes(by[2]?.outcome), `the same transaction again is refused, never queued twice ("${by[2]?.error}")`);
  check(by[3]?.outcome === Outcome.INVALID, 'bad signature → INVALID');
  check(by[4]?.outcome === Outcome.REJECTED, `contract refusal → REJECTED ("${by[4]?.error}")`);
  check(by[5]?.outcome === Outcome.INVALID, 'malformed body → INVALID');
  const tx = await (await fetch(`${BASE}/api/tx/${good.hash}`)).json();
  check(tx.hash === good.hash, 'the admitted transaction is in the ledger');
}

console.log('\ngRPC-Web over HTTP/1.1 (dev proxy, older clients)');
const h1 = createClient(LedgerService, createGrpcWebTransport({ baseUrl: BASE, httpVersion: '1.1' }));
const h1events = await take(h1.watch({}, { signal: timed(3000) }), 3000);
check(h1events.length > 3, `Watch streams over HTTP/1.1 too (${h1events.length} events in 3 s)`);

console.log('\nMarket simulator service');
const bad = await token('chaindeal-sim', 'wrong-secret', 'deals:write tx:any');
check(bad.status === 401 && bad.body.error === 'invalid_client', 'the sim client needs its own secret (wrong secret → invalid_client)');
const s1 = await (await fetch(`${BASE}/api/sim`)).json();
await new Promise((r) => setTimeout(r, 4000));
const s2 = await (await fetch(`${BASE}/api/sim`)).json();
check(typeof s1.runner === 'string' && s1.runner.startsWith('chaindeal-sim-'), `the simulator runs as its own service (${s1.runner})`);
check(s2.stats.sent > s1.stats.sent && s2.stats.admitted > s1.stats.admitted, `it streams transactions over gRPC with receipts (+${s2.stats.sent - s1.stats.sent} sent in ~4 s)`);

console.log(`\nAll ${passed} gRPC checks passed.`);
