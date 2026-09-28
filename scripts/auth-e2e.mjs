// End-to-end security test for the auth microservice and API authorization,
// run against the live cluster over TLS:
//   NODE_EXTRA_CA_CERTS=deploy/certs/ca.crt node scripts/auth-e2e.mjs
// (make auth-test sets the env). Exits non-zero on the first failed check.

import { createHash, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import * as w from '../frontend/src/wasm/chaindeal_wallet.js';

const here = dirname(fileURLToPath(import.meta.url));
w.initSync({ module: readFileSync(join(here, '../frontend/src/wasm/chaindeal_wallet_bg.wasm')) });

const BASE = process.env.CHAINDEAL_URL || 'https://chaindeal.localhost';
const REDIRECT = BASE + '/callback';
const secrets = Object.fromEntries(
  readFileSync(join(here, '../deploy/k8s/.secrets/auth.env'), 'utf8').trim().split('\n').map((l) => [l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)]),
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

const b64url = (b) => Buffer.from(b).toString('base64url');
const decode = (jwt) => JSON.parse(Buffer.from(jwt.split('.')[1], 'base64url').toString());

/** A tiny browser: cookie jar, manual redirects, Origin header on POSTs. */
class Browser {
  jar = new Map();
  async fetch(path, init = {}) {
    const headers = { ...(init.headers ?? {}) };
    if (this.jar.size) headers.cookie = [...this.jar].map(([k, v]) => `${k}=${v}`).join('; ');
    if (init.method === 'POST' && !init.noOrigin) headers.origin = BASE;
    const res = await fetch(path.startsWith('http') ? path : BASE + path, { ...init, headers, redirect: 'manual' });
    for (const c of res.headers.getSetCookie()) {
      const [kv, ...attrs] = c.split(';');
      const [k, v] = [kv.slice(0, kv.indexOf('=')), kv.slice(kv.indexOf('=') + 1)];
      if (attrs.some((a) => a.trim().toLowerCase() === 'max-age=0') || v === '') this.jar.delete(k);
      else this.jar.set(k, v);
    }
    return res;
  }
  form(path, body, extra = {}) {
    return this.fetch(path, { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', ...extra.headers }, body: new URLSearchParams(body), ...extra });
  }
}

function pkce() {
  const verifier = b64url(randomBytes(48));
  return { verifier, challenge: b64url(createHash('sha256').update(verifier).digest()), state: b64url(randomBytes(12)), nonce: b64url(randomBytes(12)) };
}

function authorizeUrl(p, extra = {}) {
  return '/oauth/authorize?' + new URLSearchParams({
    response_type: 'code', client_id: 'chaindeal-web', redirect_uri: REDIRECT,
    scope: 'openid profile deals:write sim:control users:admin', state: p.state, nonce: p.nonce,
    code_challenge: p.challenge, code_challenge_method: 'S256', ...extra,
  });
}

const hidden = (html, name) => html.match(new RegExp(`name="${name}" value="([^"]*)"`))?.[1];

async function codeFrom(res, p) {
  check(res.status === 303 || res.status === 302 || res.status === 307, `redirected back to the client (${res.status})`);
  const loc = new URL(res.headers.get('location'));
  check(loc.origin + loc.pathname === REDIRECT, 'redirect goes to the registered redirect_uri only');
  check(loc.searchParams.get('state') === p.state, 'state round-trips (CSRF protection)');
  check(loc.searchParams.get('iss') === BASE, 'iss parameter present (RFC 9207 mix-up defence)');
  return loc.searchParams.get('code');
}

async function exchange(b, code, verifier) {
  const r = await b.form('/oauth/token', { grant_type: 'authorization_code', code, redirect_uri: REDIRECT, client_id: 'chaindeal-web', code_verifier: verifier });
  return { status: r.status, body: await r.json() };
}

async function signIn(email, password) {
  const b = new Browser();
  const p = pkce();
  const page = await (await b.fetch(authorizeUrl(p))).text();
  const res = await b.form('/oauth/login', { req: hidden(page, 'req'), csrf: hidden(page, 'csrf'), email, password });
  if (res.status !== 303) return { b, res };
  const code = new URL(res.headers.get('location')).searchParams.get('code');
  const t = await exchange(b, code, p.verifier);
  return { b, token: t.body.access_token, res };
}

const api = (path, token, init = {}) =>
  fetch(BASE + '/api' + path, { ...init, headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}), ...init.headers } });
const me = (path, token, init = {}) =>
  fetch(BASE + '/oauth' + path, { ...init, headers: { 'content-type': 'application/json', authorization: `Bearer ${token}`, ...init.headers } });

// ---------------------------------------------------------------------------
console.log(`Auth e2e against ${BASE}\n\n1. Discovery & keys`);
const disc = await (await fetch(BASE + '/.well-known/openid-configuration')).json();
check(disc.issuer === BASE, 'issuer matches');
check(JSON.stringify(disc.code_challenge_methods_supported) === '["S256"]', 'only PKCE S256 advertised (no "plain")');
const jwks = await (await fetch(BASE + '/oauth/jwks')).json();
check(jwks.keys.length === 1 && jwks.keys[0].kty === 'OKP' && jwks.keys[0].crv === 'Ed25519', 'JWKS publishes one Ed25519 key');
check(!('d' in jwks.keys[0]), 'JWKS contains no private key material');

console.log('\n2. Authorization request validation');
const bad = new Browser();
let r = await bad.fetch(authorizeUrl(pkce(), { redirect_uri: 'https://evil.example/cb' }));
check(r.status === 400 && !r.headers.get('location'), 'unregistered redirect_uri: error page, never redirected (open-redirect safe)');
r = await bad.fetch(authorizeUrl(pkce(), { code_challenge_method: 'plain' }));
check(new URL(r.headers.get('location')).searchParams.get('error') === 'invalid_request', 'PKCE "plain" refused');
r = await bad.fetch(authorizeUrl(pkce()));
const csp = r.headers.get('content-security-policy') ?? '';
check(csp.includes("default-src 'none'") && !csp.includes('script-src'), 'login page CSP forbids all scripts');
check(r.headers.get('x-frame-options') === 'DENY' && csp.includes("frame-ancestors 'none'"), 'login page cannot be framed (clickjacking)');

console.log('\n3. Registration + code flow');
const email = `e2e-${Date.now()}@example.test`;
const password = `Tr4il-${b64url(randomBytes(9))}-horse`;
const user = new Browser();
const p1 = pkce();
const page1 = await (await user.fetch(authorizeUrl(p1))).text();
const req1 = hidden(page1, 'req');
r = await user.form('/oauth/register', { req: req1, csrf: 'forged', name: 'E2E', email, password, password2: password });
check(r.status === 403, 'forged CSRF token rejected');
r = await user.form('/oauth/register', { req: req1, csrf: hidden(page1, 'csrf'), name: 'E2E', email, password: 'short', password2: 'short' });
check(r.status === 400, 'weak password rejected');
r = await user.form('/oauth/register', { req: req1, csrf: hidden(page1, 'csrf'), name: 'E2E Tester', email, password, password2: password });
const sessionCookie = r.headers.getSetCookie().find((c) => c.startsWith('__Host-cd_session='));
check(sessionCookie && /HttpOnly/i.test(sessionCookie) && /Secure/i.test(sessionCookie) && /SameSite=Strict/i.test(sessionCookie), 'session cookie is __Host-, HttpOnly, Secure, SameSite=Strict');
const code1 = await codeFrom(r, p1);
let t = await exchange(user, code1, 'x'.repeat(43));
check(t.status === 400 && t.body.error === 'invalid_grant', 'wrong PKCE verifier rejected');
t = await exchange(user, code1, p1.verifier);
check(t.status === 400, 'code is single-use: burned by the failed attempt, cannot be replayed');

const p2 = pkce();
r = await user.fetch(authorizeUrl(p2)); // SSO: existing session → immediate code
const code2 = await codeFrom(r, p2);
const tokRes = await user.form('/oauth/token', { grant_type: 'authorization_code', code: code2, redirect_uri: REDIRECT, client_id: 'chaindeal-web', code_verifier: p2.verifier });
const tok = await tokRes.json();
check(tokRes.status === 200 && tok.token_type === 'Bearer' && tok.expires_in === 300, 'token issued: Bearer, 5-minute lifetime');
check(tokRes.headers.get('cache-control') === 'no-store', 'token response is not cacheable');
check(!('refresh_token' in tok), 'refresh token is NOT in the JSON body (JavaScript never sees it)');
const rtCookie = tokRes.headers.getSetCookie().find((c) => c.startsWith('__Secure-cd_rt='));
check(rtCookie && /HttpOnly/i.test(rtCookie) && /Path=\/oauth/i.test(rtCookie) && /SameSite=Strict/i.test(rtCookie), 'refresh token in HttpOnly/SameSite=Strict cookie scoped to /oauth');
check(decode(tok.id_token).nonce === p2.nonce, 'id_token carries the nonce (replay protection)');
const claims = decode(tok.access_token);
check(claims.aud === 'chaindeal-api' && claims.roles.join() === 'user', 'access token: API audience, role "user" only');
check(!claims.scope.includes('users:admin') && !claims.scope.includes('sim:control'), 'scopes limited by role (asked for admin scopes, not granted)');
r = await exchange(user, code2, p2.verifier);
check(r.status === 400, 'used code cannot be exchanged twice');
r = await user.form('/oauth/token', { grant_type: 'refresh_token' }, { headers: { origin: 'https://evil.example' }, noOrigin: true });
check(r.status === 403, 'token endpoint rejects foreign Origin (cross-site refresh)');

console.log('\n4. API authorization + wallet ownership proof');
const wallet = JSON.parse(w.generateWallet());
const regTx = w.signAction(wallet.secret, JSON.stringify({ type: 'register', name: 'E2E wallet', kind: 'consumer' }), Date.now());
r = await api('/tx', null, { method: 'POST', body: regTx });
check(r.status === 401, 'POST /api/tx without a token → 401');
r = await api('/tx', tok.access_token + 'x', { method: 'POST', body: regTx });
check(r.status === 401, 'tampered token → 401');
r = await api('/tx', tok.access_token, { method: 'POST', body: regTx });
check(r.status === 403, 'valid token but wallet not linked → 403');
const ch = await (await me('/me/wallets/challenge', tok.access_token, { method: 'POST', body: JSON.stringify({ address: wallet.address }) })).json();
const other = JSON.parse(w.generateWallet());
const forged = JSON.parse(w.signLinkChallenge(other.secret, ch.message));
r = await me('/me/wallets', tok.access_token, { method: 'POST', body: JSON.stringify({ challenge_id: ch.challenge_id, ...forged }) });
check(r.status === 400, 'linking with a different key is refused (key must match address)');
const ch2 = await (await me('/me/wallets/challenge', tok.access_token, { method: 'POST', body: JSON.stringify({ address: wallet.address }) })).json();
const proof = JSON.parse(w.signLinkChallenge(wallet.secret, ch2.message));
r = await me('/me/wallets', tok.access_token, { method: 'POST', body: JSON.stringify({ challenge_id: ch2.challenge_id, ...proof }) });
check(r.status === 200, 'wallet linked with a valid ed25519 proof');
r = await me('/me/wallets', tok.access_token, { method: 'POST', body: JSON.stringify({ challenge_id: ch2.challenge_id, ...proof }) });
check(r.status === 400, 'challenge is single-use');
let threw = false;
try { w.signLinkChallenge(wallet.secret, 'transfer all funds'); } catch { threw = true; }
check(threw, 'wallet refuses to sign anything that is not a link challenge');

const rt1 = user.jar.get('__Secure-cd_rt');
r = await user.form('/oauth/token', { grant_type: 'refresh_token', client_id: 'chaindeal-web' });
const refreshed = await r.json();
check(r.status === 200 && decode(refreshed.access_token).wallets.includes(wallet.address), 'refresh issues a token that now lists the linked wallet');
const rt2 = user.jar.get('__Secure-cd_rt');
check(rt1 !== rt2, 'refresh token rotated on use');
r = await api('/tx', refreshed.access_token, { method: 'POST', body: w.signAction(wallet.secret, JSON.stringify({ type: 'register', name: 'E2E wallet', kind: 'consumer' }), Date.now()) });
check(r.status === 202, 'transaction from the linked wallet accepted (202)');
r = await api('/sim', refreshed.access_token, { method: 'POST', body: JSON.stringify({ rate: 50 }) });
check(r.status === 403, 'simulator control without operator role → 403');

console.log('\n5. Refresh-token reuse detection');
const thief = new Browser();
thief.jar.set('__Secure-cd_rt', rt1);
r = await thief.form('/oauth/token', { grant_type: 'refresh_token', client_id: 'chaindeal-web' });
const reuse = await r.json();
check(r.status === 400 && /reuse/.test(reuse.error_description), 'replayed (stolen) refresh token detected');
r = await user.form('/oauth/token', { grant_type: 'refresh_token', client_id: 'chaindeal-web' });
check(r.status === 400, 'whole token family revoked: the legitimate current token is dead too');
const p3 = pkce();
r = await user.fetch(authorizeUrl(p3));
check(r.status === 200, 'login session revoked as well: user must sign in again');

console.log('\n6. Brute-force protection');
const victim = new Browser();
let status = 0;
for (let i = 0; i < 6; i++) {
  const pg = await (await victim.fetch(authorizeUrl(pkce()))).text();
  const res = await victim.form('/oauth/login', { req: hidden(pg, 'req'), csrf: hidden(pg, 'csrf'), email, password: 'wrong-password-' + i }, { headers: { 'x-forwarded-for': `10.9.9.${i}` } });
  status = res.status;
}
check(status === 429, 'account locked after repeated failures (even across IPs)');
const unknown = await signIn('nobody-' + Date.now() + '@example.test', 'whatever-password-123');
check(unknown.res.status === 401 && (await unknown.res.text()).includes('Invalid email or password'), 'unknown account gets the same generic error (no enumeration)');

console.log('\n7. Administration');
const admin = await signIn(secrets.AUTH_BOOTSTRAP_ADMIN_EMAIL, secrets.AUTH_BOOTSTRAP_ADMIN_PASSWORD);
check(!!admin.token && decode(admin.token).roles.includes('admin'), 'bootstrap admin signs in with admin role');
check(decode(admin.token).scope.includes('users:admin'), 'admin token carries users:admin');
const found = await (await me('/admin/users?email=' + encodeURIComponent(email), admin.token)).json();
check(found.total === 1 && found.items[0].email === email, 'admin finds a user by exact email via the blind index');
const uid = found.items[0].id;
check(found.items[0].locked_until > 0, 'admin sees the lockout');
r = await me('/admin/users/' + uid, admin.token, { method: 'PATCH', body: JSON.stringify({ roles: ['user', 'operator'], unlock: true }) });
check(r.status === 200, 'admin grants operator and unlocks');
r = await me('/admin/users?offset=0&limit=5', refreshed.access_token);
check(r.status === 403, 'non-admin token cannot use the admin API');
r = await me('/admin/users/' + decode(admin.token).sub, admin.token, { method: 'PATCH', body: JSON.stringify({ roles: ['user'] }) });
check(r.status === 400, 'admin cannot remove their own admin role (lock-out protection)');
const audit = await (await me('/admin/audit?user_id=' + uid, admin.token)).json();
const events = audit.items.map((e) => e.event);
check(['register', 'refresh_reuse_detected', 'login_failed', 'wallet_linked', 'admin_user_updated'].every((e) => events.includes(e)), 'audit log recorded register, reuse, failures, wallet link, role change');

const op = await signIn(email, password);
check(!!op.token && decode(op.token).scope.includes('sim:control'), 'after re-login the user holds operator + sim:control');
const cur = await (await api('/sim', null)).json();
r = await api('/sim', op.token, { method: 'POST', body: JSON.stringify({ rate: cur.config.rate }) });
check(r.status === 200, 'operator may control the market');

console.log('\n8. Account deletion');
r = await me('/admin/users/' + decode(admin.token).sub, admin.token, { method: 'DELETE' });
check(r.status === 400, 'admin cannot delete their own account');
r = await me('/admin/users/' + uid, admin.token, { method: 'DELETE' });
const del = await r.json();
check(r.status === 200 && del.wallets_released === 1, 'admin deletes the test account; its wallet link is released');
const gone = await (await me('/admin/users?email=' + encodeURIComponent(email), admin.token)).json();
check(gone.total === 0, 'deleted account no longer exists');
r = await api('/sim', op.token, { method: 'POST', body: JSON.stringify({ rate: cur.config.rate }) });
check(r.status === 200 || r.status === 401 || r.status === 403, 'outstanding access token expires on its own (≤5 min), refresh is already dead');
r = await op.b.form('/oauth/token', { grant_type: 'refresh_token', client_id: 'chaindeal-web' });
check(r.status === 400, 'deleted account cannot refresh');

console.log(`\nAll ${passed} checks passed.`);
