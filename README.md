# ChainDeal

**Smart escrow for C2C, C2B, B2C and B2B deals, as a working blockchain.**
Individuals and businesses agree terms, lock value in on-chain custody, and release it when both sides are satisfied or an arbiter rules. Every step is an ed25519-signed transaction, sealed into proof-of-work blocks and stored in a replicated Tarantool cluster on Kubernetes.

![Live market: every deal moving through the contract in real time](docs/screenshots/09-live-market.png)

| | |
|---|---|
| **Backend** | Rust (Axum, Tokio): chain nodes with leader election, block production and a live market simulator |
| **Identity** | Rust `chaindeal-auth` microservice: OAuth 2.1 / OpenID Connect, PKCE, rotating refresh tokens, user management |
| **Database** | Tarantool 3: a replicated set for the chain (master + read replica) and a separate instance for identity |
| **Frontend** | React + TypeScript, plus a **Rust wallet compiled to WebAssembly** that signs and verifies in the browser with the node's own code |
| **Platform** | Docker Desktop Kubernetes (3 nodes), Traefik TLS ingress, NetworkPolicies, `restricted` Pod Security |
| **Scale** | 1,000,000+ transactions of history, 214k deals, 6,000 agents, and a live market at 10 tx/s |

## Contents

1. [Screenshots](#screenshots)
2. [Architecture](#architecture)
3. [Quick start](#quick-start)
4. [Repository layout](#repository-layout)
5. [How it works: code walkthrough](#how-it-works-code-walkthrough)
6. [Security model](#security-model)
7. [Operations](#operations)
8. [API reference](#api-reference)
9. [Configuration](#configuration)
10. [Testing](#testing)
11. [Limitations](#limitations)

---

## Screenshots

All screenshots show the real system: the live cluster with the 1M-transaction history, driven by a script (`make screenshots`) that signs up, links wallets and navigates like a user would.

### Deals and the Custody Bridge

On the **Desk**, the first thing you see is what needs your move. Network state fits on one ruled "ledger line".

![Desk](docs/screenshots/03-desk.png)

The **Custody Bridge** is the signature view: value is an ingot whose *position is the contract state*. Here, 348.99 DEAL is locked in the vault while the buyer inspects the goods; the seal counts down the inspection window.

![Deal in custody](docs/screenshots/05-deal-bridge-custody.png)

| Arbiter ruling: custody split 40/60, bond returned | Seller missed the delivery deadline: deal failed, buyer refunded and bond forfeited |
|---|---|
| ![Ruling](docs/screenshots/06-deal-bridge-ruling-split.png) | ![Failed](docs/screenshots/07-deal-bridge-failed.png) |

| Deals docket (funds track: buyer · custody · seller) | Proposing a deal: the contract preview is computed in WASM |
|---|---|
| ![Docket](docs/screenshots/04-deals-docket.png) | ![New deal](docs/screenshots/08-new-deal-contract-preview.png) |

### Live market

Every state change sealed in a block flies across the state machine as a particle: brass while in progress, green when settled, red when a deal fails. Counts are live totals across the whole chain. The controls change the simulator cluster-wide and require the `operator` role.

![Live market, full page](docs/screenshots/10-live-market-full.png)

### Ledger, directory and wallet

| Blocks as strata (thickness = transactions) | A block re-verified *in the browser* by the Rust/WASM wallet |
|---|---|
| ![Ledger](docs/screenshots/12-ledger-strata.png) | ![Verified](docs/screenshots/13-block-verified-in-browser.png) |

| Directory (search across 6,000 participants, reputation incl. missed deadlines) | Wallet (keys stay in the browser; wallets linked to your account) |
|---|---|
| ![Directory](docs/screenshots/14-directory.png) | ![Wallet](docs/screenshots/15-wallet-linked.png) |

### Identity: sign-in, account, administration

| Hosted sign-in (served by the auth service, no JavaScript) | Registration |
|---|---|
| ![Sign in](docs/screenshots/01-hosted-sign-in.png) | ![Register](docs/screenshots/02-hosted-register.png) |

| Account: profile, password, linked wallets, sessions | Cluster panel: API nodes, leader, Tarantool replica set |
|---|---|
| ![Account](docs/screenshots/16-account.png) | ![Cluster](docs/screenshots/11-cluster-panel.png) |

The **Admin** page lets you manage users, roles, suspension, unlocking and deletion, and read the security audit log:

![Admin](docs/screenshots/17-admin-users-audit.png)

### Dark mode and mobile

| Dark ("vault room") | Mobile |
|---|---|
| ![Dark deal](docs/screenshots/18-dark-deal-bridge.png) ![Dark live](docs/screenshots/19-dark-live-market.png) | ![Mobile deal](docs/screenshots/20-mobile-deal.png) ![Mobile docket](docs/screenshots/21-mobile-docket.png) |

> The mobile shots show **Sign in** in the header because the capture script copied browser state into a third browser. That reused an already-rotated refresh token, and the auth service's reuse detection revoked the session, exactly as designed (see [Security model](#security-model)).

---

## Architecture

```
                              https://chaindeal.localhost
                                           │  :80 → 301 → :443 (TLS, HTTP/2)
┌──────────────────────────────── Docker Desktop Kubernetes (3 nodes) ───────────────────────────────┐
│  namespace ingress          ┌────────────────────────────────────────┐                             │
│                             │  Traefik ×2  (Docker Desktop LoadBalancer) │                             │
│                             └───────┬───────────────┬───────────┬─────┘                             │
│  namespace chaindeal         /      │      /oauth   │    /api   │   (NetworkPolicies: default deny)   │
│              ┌──────────────────────▼┐  ┌───────────▼──────┐ ┌──▼────────────────────────────────┐   │
│              │ frontend ×2 (nginx)   │  │ auth ×2          │ │ backend ×3 (Rust chain nodes)     │   │
│              │ CSP · HSTS · SPA      │  │ OAuth 2.1 / OIDC │◀┤ lease → 1 leader mines + simulates│   │
│              └───────────────────────┘  │ users · wallets  │ │ verifies tokens via JWKS          │   │
│                                         └────────┬─────────┘ └────┬─────────────────┬───────────┘   │
│                                                  │ only auth      │ writes          │ reads         │
│                                         ┌────────▼────────┐ ┌─────▼───────────┐ ┌──▼──────────────┐ │
│                                         │ auth-db         │ │ tarantool-0     │─▶│ tarantool-1     │ │
│                                         │ (identity only) │ │ master (rw)     │WAL│ replica (ro)    │ │
│                                         └─────────────────┘ └─────────────────┘ └─────────────────┘ │
└────────────────────────────────────────────────────────────────────────────────────────────────────┘
      Browser: React UI + wallet.wasm (Rust: keys, signing, contract preview, block verification)
```

Replicas are spread across the two worker nodes, and the spread is required, not merely preferred:

```
$ kubectl get pods -A -o custom-columns=NAMESPACE:.metadata.namespace,POD:.metadata.name,NODE:.spec.nodeName
chaindeal   auth-54f87d6548-bvcrx       desktop-worker
chaindeal   auth-54f87d6548-wt2jc       desktop-worker2
chaindeal   auth-db-0                   desktop-worker2
chaindeal   backend-7cbd749485-5dvtm    desktop-worker2
chaindeal   backend-7cbd749485-6gdmg    desktop-worker
chaindeal   backend-7cbd749485-d8l42    desktop-worker
chaindeal   frontend-6dd6bd46c-2c7pw    desktop-worker
chaindeal   frontend-6dd6bd46c-8cd78    desktop-worker2
chaindeal   tarantool-0                 desktop-worker
chaindeal   tarantool-1                 desktop-worker2
ingress     traefik-58758ffdd7-7ztqf    desktop-worker2
ingress     traefik-58758ffdd7-smsc6    desktop-worker
```

**Transaction lifecycle.**
1. The browser signs an action with the WASM wallet and sends it with a Bearer token.
2. Any backend pod verifies the token (JWKS) and the ed25519 signature, dry-runs the contract on top of the mempool, and queues the transaction in Tarantool.
3. Every 2 s, the **leader** drains the mempool. It applies each transaction with the same contract code, mines a PoW block, and commits the block, transactions, accounts and deals in **one** Tarantool transaction.
4. The block event goes onto a shared log that every pod tails, so SSE clients on any pod see it.

---

## Quick start

**Prerequisites:** Docker Desktop with Kubernetes enabled (Settings → Kubernetes → *Enable*, provisioning method **kind**, **3 nodes**). Also rustup, Node 20 and OpenSSL. Tarantool never runs directly on your machine, only in the cluster.

```bash
make setup     # once: wasm32 target, wasm-pack, npm deps, builds the WASM wallet
make k8s-up    # ingress, TLS, secrets, images, Tarantool set, 1M-tx history, app (~6 min)
make k8s-trust # prints the one command that trusts the local CA in your keychain (you run it)
make k8s-status
```

Open **https://chaindeal.localhost**. `*.localhost` always resolves to your own machine (RFC 6761), so no DNS or `/etc/hosts` changes are needed. `make k8s-status` prints the bootstrap administrator's credentials.

Development against the cluster:

```bash
make web          # Vite on http://localhost:5173, proxying /api, /oauth, /.well-known to the cluster over TLS
make seed         # demo parties (Alice, Bob, Acme, Globex, TrustCourt) via the client_credentials grant
make screenshots  # regenerate docs/screenshots with local Chrome
```

On the Wallet page choose **Import demo wallets**, sign in, and **Link** each wallet to your account (a signature proof). Now you can act as any party.

---

## Repository layout

```
crates/core/          Shared Rust: types, canonical signing, PoW/Merkle, the escrow contract (+ unit tests)
crates/authn/         EdDSA JWT issue/verify, JWKS types, wallet-link message (used by auth and backend)
crates/wallet-wasm/   wasm-bindgen exports: keygen, signing, link proofs, contract quotes, block verification
backend/src/
  main.rs             node bootstrap: connections, leader/bus/producer/simulator tasks, HTTP server
  api.rs              chain REST API + SSE, token authorization, security headers
  authz.rs            access-token verification against the auth service's JWKS
  chain.rs            admission, block production, parallel PoW, leader lease, event bus, metrics, verification
  db.rs               Tarantool client (master + read pool), JSON⇄MessagePack bridge
  sim.rs              live market simulator (leader-only, config shared via Tarantool)
  synth.rs            synthetic economy: agents, catalogs, scenario mix, the deal "director"
  bin/chaindeal-bulk.rs   1M-transaction history generator (a real signed, mined chain)
auth/src/
  main.rs             identity service bootstrap, routes, security headers, bootstrap admin
  oauth.rs            discovery, JWKS, authorize, login/register, token, logout, userinfo
  account.rs          self-service (profile, password, wallets, sessions) and admin API
  app.rs              sessions, token issuance, scopes, rate limiting, cookies, password policy
  crypto.rs           Argon2id, AES-256-GCM + blind index, PKCE, token hashing
  pages.rs            hosted sign-in/registration HTML (script-free)
auth/tarantool/init.lua   identity schema: users, wallets, codes, refresh, sessions, audit + sweeper
tarantool/init.lua    chain schema, stored procedures, counters, replication, cluster primitives
frontend/src/
  lib/wasm.ts         typed façade over the WASM wallet
  lib/auth.tsx        OAuth 2.1 PKCE client (memory tokens, silent refresh)
  lib/store.tsx       app state: wallets, lazy account cache, SSE with watchdog, signed submissions
  components/Bridge.tsx   the Custody Bridge (signature view) + MoneyTrack
  pages/              Desk, Deals, DealDetail, NewDeal, Live, Explorer, Accounts, Wallet, Account, Admin
frontend/scripts/screenshots.mjs   drives Chrome through the product to produce docs/screenshots
deploy/
  certs.sh            name-constrained local CA + TLS certificate
  nginx/              frontend server config and security headers
  k8s/ingress/        Traefik (RBAC, Deployment, LoadBalancer, PDB)
  k8s/base/           namespace, Tarantool StatefulSets, auth-db, secrets, NetworkPolicies
  k8s/app/            backend, auth, frontend Deployments, PDBs, Ingress
  k8s/jobs/bulk.yaml  history loader Job
scripts/
  seed.mjs            demo parties + deals (machine OAuth client)
  auth-e2e.mjs        61 end-to-end security checks against the live cluster
```

---

## How it works: code walkthrough

### 1. One Rust core, two targets

`crates/core` compiles natively into the node and to WebAssembly for the browser, so both sides agree byte-for-byte on what gets signed. A transaction body is serialized canonically, hashed with SHA-256, and the hash is signed with ed25519. An address is `0x` followed by the first 20 bytes of `sha256(pubkey)` ([crypto.rs](crates/core/src/crypto.rs)):

```rust
pub fn sign_action(secret_hex: &str, action: Action, nonce: u64, timestamp_ms: u64) -> Result<SignedTx, CryptoError> {
    let key = signing_key(secret_hex)?;
    let pk = key.verifying_key().to_bytes();
    let body = TxBody { from: address_from_pubkey(&pk), pubkey: hex::encode(pk), nonce, timestamp: timestamp_ms, action };
    let hash = tx_hash(&body);
    let sig = key.sign(hash.as_bytes());
    Ok(SignedTx { hash, body, signature: hex::encode(sig.to_bytes()) })
}
```

The browser uses the same code through [lib/wasm.ts](frontend/src/lib/wasm.ts). It also verifies blocks independently: it re-derives the hash, the PoW and the Merkle root, and checks every signature, without trusting the node.

### 2. The escrow contract

[contract.rs](crates/core/src/contract.rs) is a pure, deterministic state machine that both the node and the bulk loader run:

```
Proposed ─accept→ Accepted ─fund→ Funded ─deliver→ Delivered ─confirm / claim after window→ Completed
 │ │ └───cancel────────┴──cancel*───┘ │ └──dispute──┴──dispute→ Disputed ─arbiter ruling→ Resolved
 │ └─decline→ Declined                │
 └─deadline→ Expired ←─deadline──(Accepted: buyer never funded)
                                      └─deadline→ Failed (seller never delivered: refund + bond slashed)
```

Rules depend on the deal type, which is derived from the parties (seller kind → buyer kind):

| Type | Fee | Seller bond | Accept / fund / deliver within | Inspection | Arbiter | Buyer may withdraw after funding |
|---|---|---|---|---|---|---|
| C2C | 1% | — | 3 / 3 / 5 min | 2 min | optional | no |
| C2B | 1% | — | 3 / 3 / 5 min | 3 min | optional | no |
| B2C | 1.5% | — | 3 / 4 / 5 min | 5 min | required | **until delivery** |
| B2B | 0.5% | **10%** | 4 / 4 / 7 min | 4 min | required | no |

- **Deadlines.** Once a deadline passes, that step can no longer be taken; anyone involved can send `expire_deal`. This ends the deal as Expired (buyer default) or Failed (seller default: refund plus bond slashing). Each default is recorded as a `defaults` reputation point on the account.
- **Money is conserved.** Unit tests check that the total supply is unchanged across every path.
- **Rollback costs O(1).** It snapshots only the entries a transaction can touch, never the whole state, which matters with 1M transactions in play.

### 3. Blocks and storage

[chain.rs](backend/src/chain.rs):

- **Admission.** Checks the signature and freshness, then dry-runs the contract on top of the current mempool, so actions that would obviously fail are refused with HTTP 422.
- **Block production.** Re-validates every transaction and records failures as `rejected`. It builds the Merkle root and mines PoW across 4 threads, with each thread striding through the nonce space.
- **Atomic commit.** The whole block commits through one stored procedure, which also refuses anything that doesn't extend the tip ([tarantool/init.lua](tarantool/init.lua)):

```lua
function cd_commit_block(block, txs, accounts, deals)
    box.atomic(function()
        if block ~= nil then
            local h = block.header.height
            local tip = box.space.blocks.index.primary:max()
            if tip then
                if h ~= tip.height + 1 or block.header.prev_hash ~= tip.hash then
                    box.error({ reason = string.format('block %d does not extend tip %d', h, tip.height) })
                end
            ...
```

Designed for 1M rows:
- Deal statistics are **counters** maintained inside that same transaction.
- Filtered lists read a `(status, updated_at)` **index**. Tarantool 3 aborts long-running Lua (`fiber slice exceeded`), so scans are avoided by design.
- Verification runs in ranges of up to 500 blocks, which the UI walks with a progress bar: about 50 s and ~19k signatures/s for the whole chain.

### 4. Clustering

Any number of stateless API nodes share one Tarantool master:

- **Leader election** uses a lease row in Tarantool (TTL 6 s, renewed every 2 s). The leader mines and runs the simulator. On SIGTERM it hands the lease back, so failover takes about 1.3 s; after a crash, the lease expires within 6 s.

```lua
function cd_lease(name, holder, ttl)
    local now = clock.time()
    local t = box.space.leases:get(name)
    if t == nil or t.holder == holder or t.expires < now then
        box.space.leases:replace({ name, holder, now + ttl })
        return true
    end
    return false
end
```

- **Shared event bus.** Each node publishes its events to an `events` space and tails it every 200 ms into its own SSE subscribers. A browser connected to *any* pod sees blocks produced by the leader.
- **Shared metrics and settings.** Per-second throughput counters are upserted by every node. Simulator config and snapshots live in a `kv` space, so the controls work through any pod.
- **Read scaling.** List, search, stats and verification queries use `tarantool-read` (master + replica). Writes use `tarantool-rw`. The replica streams the master's WAL; lag is shown in the Cluster panel.

### 5. Synthetic economy, bulk history and live market

[synth.rs](backend/src/synth.rs) defines 6,000 agents: 40 arbiters, 1,200 businesses and 4,760 individuals. Their keys derive from their index, so no secrets are stored.

- **Scenarios.** Each new deal draws a scenario from a weighted mix: completion, seller claim, decline, lapse, withdrawal, buyer default, seller default, and disputes won by either side or split.
- **The director.** A function that reads the deal's **actual** on-chain state and returns the next step. A refused transaction (for example, insufficient funds) naturally turns into a failure path.
- **Two consumers.**
  - [chaindeal-bulk](backend/src/bin/chaindeal-bulk.rs) writes a week of history: 1M transactions in about 190 s, as real signed and mined blocks.
  - [sim.rs](backend/src/sim.rs) runs on the leader at 10 tx/s through the normal admission path, adopting open deals after restarts or failover.

### 6. Identity service and API authorization

The browser flow (see [oauth.rs](auth/src/oauth.rs) and [lib/auth.tsx](frontend/src/lib/auth.tsx)):

```
SPA ──/oauth/authorize (PKCE S256, state, nonce)──▶ hosted sign-in ──303──▶ /callback?code&state&iss
SPA ──/oauth/token (code + code_verifier)──▶ access token (5 min, memory) + refresh token (HttpOnly cookie)
SPA ──/api/tx  Authorization: Bearer …──▶ backend verifies EdDSA JWT via JWKS, scope and wallet ownership
```

Refresh tokens rotate on every use. Presenting an already-used token (theft) revokes the whole family and the login session, atomically ([auth/tarantool/init.lua](auth/tarantool/init.lua)):

```lua
function auth_rotate_refresh(old_key, new_key, new_expires)
    ...
        if doc.used then
            for _, r in box.space.refresh.index.idx:pairs({ t.idx }) do box.space.refresh:delete(r.key) end
            result = { status = 'reuse', family = t.idx, user_id = doc.user_id, sid = doc.sid }
            return
        end
        ... mark old used, insert the new token in the same family ...
```

The chain API ties the **account** (token) to the **signer** (ed25519) ([api.rs](backend/src/api.rs)):

```rust
// The ed25519 signature proves who *signed*; the access token proves which
// *account* is acting, and that account must own the signing wallet.
let c = authz.claims(&headers).await?;
if !c.has_scope("deals:write") { return Err(forbid("deals:write scope required").into()); }
let owns = c.wallets.iter().any(|w| w == &tx.body.from);
if !owns && !c.has_scope("tx:any") { return Err(forbid("this wallet is not linked to your account …").into()); }
```

**Wallet linking** proves possession of the key:
1. The auth service issues a one-time challenge.
2. The WASM wallet signs it (`signLinkChallenge`). It refuses to sign anything that isn't a domain-separated link message.
3. The service checks that the key hashes to the address and that the signature verifies.

### 7. Frontend

- **[Bridge.tsx](frontend/src/components/Bridge.tsx).** The Custody Bridge is a pure function of the deal. `fundsPosition()` in [lib/deal.ts](frontend/src/lib/deal.ts) maps contract state to where the value sits. CSS transitions animate the ingot only when a live block changes the state; the seal stamps only then, never on page load.
- **[Live.tsx](frontend/src/pages/Live.tsx).** Draws the state-machine graph in SVG with a DPR-aware canvas layer for particles. Transitions from each block are spread across the block interval. It respects `prefers-reduced-motion`.
- **[store.tsx](frontend/src/lib/store.tsx).** A lazily filled account cache that refreshes only the accounts a block touched. SSE events are buffered, and a watchdog reconnects a stalled stream.

---

## Security model

| Area | Measures |
|---|---|
| **Transport** | TLS everywhere at the edge (HTTP/2), permanent HTTP→HTTPS redirect, HSTS. Local CA with **X.509 name constraints**: it can only ever sign `localhost` / `*.localhost` / `*.test` names |
| **Browser** | Strict CSP (`script-src 'self' 'wasm-unsafe-eval'`, `frame-ancestors 'none'`), `nosniff`, `DENY` framing, COOP/CORP, Permissions-Policy. Hosted sign-in pages allow **no scripts at all** |
| **Authentication** | OAuth 2.1 Authorization Code + **PKCE S256** only, exact redirect URIs, `state`, `nonce`, RFC 9207 `iss`. Codes are single-use (60 s) |
| **Tokens** | 5-min **EdDSA** access tokens in memory only; the verifier accepts EdDSA alone (no `alg:none` or algorithm confusion). Refresh tokens are **rotated**, kept in `HttpOnly; Secure; SameSite=Strict; Path=/oauth` cookies, and **reuse** revokes the session. Origin checks on the token endpoint |
| **Credentials** | **Argon2id** (19 MiB, t=2) with bounded concurrency. Password policy of ≥12 chars plus blocklist. Account lockout with exponential backoff and per-IP limits. Same error and work for unknown accounts (no enumeration) |
| **Data at rest** | Emails **AES-256-GCM** (bound to the record) with an HMAC blind index. Codes, sessions and refresh tokens stored only as **SHA-256 hashes** |
| **Authorization** | Scopes derived from roles (`user`, `operator`, `admin`). Admin actions re-check roles in the database. Role changes, suspension and deletion sign the user out everywhere. Transactions must come from **linked** wallets |
| **Cluster** | `restricted` Pod Security in both namespaces. Non-root, read-only root filesystems, all capabilities dropped, no service-account tokens. **NetworkPolicies**: default deny; only the API and bulk job reach the chain DB; only auth reaches `auth-db` |
| **Secrets** | Randomly generated into `deploy/k8s/.secrets/` (mode 600, gitignored) and mounted as Kubernetes Secrets. Demo private keys are stripped from images |
| **Audit** | Logins, failures, lockouts, token reuse, wallet links, password and admin changes, visible on the Admin page |

---

## Operations

| Command | What it does |
|---|---|
| `make k8s-up` | Everything: ingress, certificate, secrets, images, data tier, 1M history, app |
| `make k8s-cluster` / `k8s-data` / `k8s-bulk` / `k8s-app` | The individual stages |
| `make k8s-images` | Build backend, auth and frontend images and import them into every node's containerd |
| `make k8s-status` | Pods, URL and bootstrap admin credentials |
| `make k8s-trust` | Prints the command to trust (or remove) the local CA |
| `make k8s-down` | Remove ChainDeal from the cluster (data included; the cluster stays) |
| `make db-reset` | Wipe the chain in-cluster and reload the history (`TXS=…`) |
| `make db-forward` / `make backend` | Port-forward the Tarantool master and run an extra node locally (it joins as a follower) |
| `make web` / `make seed` / `make screenshots` | Dev UI, demo data, documentation screenshots |
| `make test` / `make auth-test` | Unit tests + type check / 61 live security checks |

Try a failover: open the Live page, then `kubectl -n chaindeal delete pod <leader>`. The Cluster panel shows another node take over within about 1.3 s, and blocks keep coming.

---

## API reference

Chain API (`/api`, served by any backend pod):

| Method | Path | |
|---|---|---|
| GET | `/api/health`, `/api/stats`, `/api/metrics`, `/api/policies` | node health, counters, per-second throughput, deal rules |
| POST | `/api/tx` | submit a signed transaction (**Bearer**, `deals:write`, sender wallet linked) → `202` / `401` / `403` / `400` / `422` |
| GET | `/api/tx/:hash`, `/api/txs` | transaction status, recent transactions |
| GET | `/api/blocks`, `/api/blocks/:height-or-hash` | block headers; block with transactions |
| GET | `/api/deals?type=&status=&limit=&offset=`, `/api/deals/:id` | deals (`status=open` = any non-terminal) |
| GET | `/api/accounts?q=&kind=`, `/api/accounts/lookup?addrs=`, `/api/accounts/:addr` | search, batch lookup, profile |
| GET | `/api/chain/verify?from=&count=&prev=` | verify a block range (≤500) |
| GET / POST | `/api/sim` | simulator state / change (**Bearer**, `sim:control`, operator role) |
| GET | `/api/cluster` | API nodes, leader, Tarantool instances and replication lag |
| GET | `/api/events` | SSE: `pending`, `refused`, `rejected`, `block` (with deal transitions), `leader` |

Identity API (`/oauth`, `/.well-known`, served by the auth service):

| Method | Path | |
|---|---|---|
| GET | `/.well-known/openid-configuration`, `/oauth/jwks` | discovery, public keys |
| GET | `/oauth/authorize` | start sign-in (PKCE S256 required) |
| POST | `/oauth/token` | `authorization_code`, `refresh_token` (cookie), `client_credentials` |
| POST | `/oauth/logout` | end the session and its refresh tokens |
| GET | `/oauth/userinfo`, `/oauth/me` | identity; full profile with wallets and sessions |
| POST / DELETE | `/oauth/me/password`, `/oauth/me/wallets[/challenge]`, `/oauth/me/sessions/:id`, `/oauth/me/logout-all` | self-service |
| GET / PATCH / DELETE | `/oauth/admin/users[/:id]`, `/oauth/admin/audit` | administration (`users:admin` + admin role) |

---

## Configuration

| Variable | Default | Used by |
|---|---|---|
| `TARANTOOL_ADDR` / `TARANTOOL_READ_ADDR` | `127.0.0.1:3301` / unset | backend (master / read pool) |
| `CHAINDEAL_BLOCK_MS`, `CHAINDEAL_DIFFICULTY` | `2000`, `4` | backend |
| `CHAINDEAL_SIM`, `CHAINDEAL_SIM_RATE`, `CHAINDEAL_SIM_PRESSURE`, `CHAINDEAL_SIM_AGENTS` | `on`, `10`, `1`, `6000` | backend simulator |
| `AUTH_JWKS_URL`, `AUTH_ISSUER` | `http://auth:8080/oauth/jwks`, `https://chaindeal.localhost` | backend token verification |
| `CHAINDEAL_AUTH` | `on` (`off` = unprotected, loud warning) | backend |
| `CHAINDEAL_CORS_ORIGINS` | unset (same-origin only) | backend |
| `AUTH_DB_ADDR`, `AUTH_SIGNING_KEY`, `AUTH_DATA_KEY`, `AUTH_INDEX_KEY` | Secret | auth |
| `AUTH_WEB_REDIRECTS`, `AUTH_SEED_CLIENT_SECRET`, `AUTH_BOOTSTRAP_ADMIN_*` | Deployment / Secret | auth |
| `TT_MEMTX_MEMORY_MB`, `TT_REPLICATION_SOURCE` | `1024` (cluster: `2048`), unset = master | Tarantool |

---

## Testing

- **`make test`** runs 16 Rust unit tests and the TypeScript type check:
  - **Contract (12):** every lifecycle path, including the failure model, and conservation of supply.
  - **JWT (1):** tampering, `alg:none`, unknown keys, expiry and audience.
  - **Auth crypto (3):** the RFC 7636 PKCE vector, record-bound encryption and Argon2id.
- **`make auth-test`** runs 61 end-to-end checks against the live cluster over TLS:
  - open redirects, PKCE `plain`, CSRF and code replay;
  - cookie attributes, token reuse and brute-force lockout;
  - no account enumeration, wallet-proof forgery, and role enforcement;
  - admin self-lockout protection, account deletion and the audit trail.
- **`make seed`** is also an end-to-end test of the contract and the machine OAuth client.
- **Verify entire chain** on the Ledger page re-derives every hash, link, Merkle root and signature.

---

## Limitations

- **Single-node chain.** No networking or consensus between independent nodes (the API nodes share one database); accounts are funded by a faucet. Deadlines are minutes long so the demo moves.
- **Database failover.** Tarantool master failover is manual, and `auth-db` is a single instance.
- **Network exposure.** Docker Desktop publishes the LoadBalancer on **all interfaces**, so the site is reachable from your LAN. Block incoming connections for Docker in the macOS firewall, or front it with a localhost-only forwarder.
- **Client IPs.** Docker Desktop's load balancer hides the real client address, so per-IP rate limits apply per node rather than per client. Account lockout is unaffected.
- **Auth gaps.** No MFA and no email verification. Access tokens aren't revocable before their 5-minute expiry. Signing-key rotation isn't automated (the JWKS refresh supports it).
- **Internal traffic.** Traffic between pods is plaintext, fenced by NetworkPolicies but without mTLS.
