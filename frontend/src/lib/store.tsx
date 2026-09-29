import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { api, ApiError } from './api';
import { currentClaims } from './auth';
import { ACTION_LABEL, STATUS_LABEL } from './format';
import { ledger, toNodeEvent, type NodeEvent } from './ledger';
import type { Account, Action, PartyKind, TxRecord } from './types';
import { signAction, walletFromSecret, type Keypair } from './wasm';

export interface LocalWallet extends Keypair { label: string; kind?: PartyKind }

export interface FeedItem { id: number; at: number; kind: 'block' | 'rejected' | 'pending' | 'refused' | 'transition'; text: string; link?: string }
export interface Toast { id: number; tone: 'info' | 'ok' | 'err'; text: string }

export type { NodeEvent, StreamTx } from './ledger';

/** State of the gRPC-Web event stream (read on demand; changes ~10×/s). */
export interface StreamInfo { up: boolean; seq: bigint; events: number; reconnects: number; since: number }

interface Ctx {
  wallets: LocalWallet[];
  active: LocalWallet | null;
  me: Account | null;
  setActive(address: string): void;
  addWallet(w: LocalWallet): void;
  removeWallet(address: string): void;
  /** Lazily populated account cache (the network has thousands of accounts). */
  directory: Map<string, Account>;
  /** Ensures these accounts are fetched into the cache. */
  resolve(addrs: string[]): void;
  nameOf(address: string | null | undefined): string;
  height: number;
  /** Bumps whenever a new block lands; pages refetch on change. */
  version: number;
  feed: FeedItem[];
  toasts: Toast[];
  toast(tone: Toast['tone'], text: string): void;
  /** Signs with the active wallet, submits and resolves once confirmed in a block. */
  submit(action: Action, opts?: { wallet?: LocalWallet }): Promise<TxRecord>;
  busy: boolean;
  /** Subscribe to raw node events; returns an unsubscribe function. */
  subscribe(fn: (e: NodeEvent) => void): () => void;
  /** Current state of the live stream. */
  streamInfo(): StreamInfo;
}

const C = createContext<Ctx | null>(null);
export const useStore = () => useContext(C)!;

const LS_WALLETS = 'chaindeal.wallets';
const LS_ACTIVE = 'chaindeal.active';
const FEED_FLUSH_MS = 400;

function load<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v ? (JSON.parse(v) as T) : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v));
  } catch {
    /* storage unavailable: wallets live for this session only */
  }
}

type Waiter = { resolve(r: TxRecord): void; reject(e: Error): void };

export function StoreProvider({ children }: { children: ReactNode }) {
  const [wallets, setWallets] = useState<LocalWallet[]>(() =>
    load<LocalWallet[]>(LS_WALLETS, []).filter((w) => {
      try {
        return walletFromSecret(w.secret).address === w.address;
      } catch {
        return false;
      }
    }),
  );
  const [activeAddr, setActiveAddr] = useState<string | null>(() => load<string | null>(LS_ACTIVE, null));
  const [directory, setDirectory] = useState<Map<string, Account>>(new Map());
  const [height, setHeight] = useState(0);
  const [version, setVersion] = useState(0);
  const [feed, setFeed] = useState<FeedItem[]>([]);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [inflight, setInflight] = useState(0);
  const waiters = useRef(new Map<string, Waiter>());
  const listeners = useRef(new Set<(e: NodeEvent) => void>());
  const seq = useRef(0);
  const stream = useRef<StreamInfo>({ up: false, seq: 0n, events: 0, reconnects: 0, since: Date.now() });

  // --- lazy account cache ------------------------------------------------------
  const known = useRef(new Set<string>()); // requested at least once
  const queue = useRef(new Set<string>());
  const flushTimer = useRef<number | null>(null);

  const flush = useCallback(async () => {
    flushTimer.current = null;
    const batch = [...queue.current];
    queue.current.clear();
    for (let i = 0; i < batch.length; i += 200) {
      const got = await api.lookup(batch.slice(i, i + 200)).catch(() => [] as Account[]);
      if (got.length) {
        setDirectory((prev) => {
          const next = new Map(prev);
          for (const a of got) next.set(a.address, a);
          return next;
        });
      }
    }
  }, []);

  /** Queue addresses for fetching; `force` refreshes ones already cached. */
  const enqueue = useCallback(
    (addrs: string[], force = false) => {
      for (const a of addrs) {
        if (!a || (!force && known.current.has(a))) continue;
        known.current.add(a);
        queue.current.add(a);
      }
      if (queue.current.size && flushTimer.current == null) flushTimer.current = window.setTimeout(flush, 30);
    },
    [flush],
  );

  useEffect(() => save(LS_WALLETS, wallets), [wallets]);
  useEffect(() => save(LS_ACTIVE, activeAddr), [activeAddr]);
  useEffect(() => enqueue(wallets.map((w) => w.address), true), [wallets, enqueue]);

  const active = wallets.find((w) => w.address === activeAddr) ?? wallets[0] ?? null;

  const toast = useCallback((tone: Toast['tone'], text: string) => {
    const id = ++seq.current;
    setToasts((t) => [...t.slice(-3), { id, tone, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), tone === 'err' ? 7000 : 4000);
  }, []);

  // --- live feed, buffered so 10+ events/s don't re-render the app 10×/s -------
  const feedBuf = useRef<FeedItem[]>([]);
  useEffect(() => {
    const t = setInterval(() => {
      if (!feedBuf.current.length) return;
      const items = feedBuf.current.reverse();
      feedBuf.current = [];
      setFeed((prev) => [...items, ...prev].slice(0, 40));
    }, FEED_FLUSH_MS);
    return () => clearInterval(t);
  }, []);
  const pushFeed = useCallback((f: Omit<FeedItem, 'id' | 'at'>) => {
    feedBuf.current.push({ ...f, id: ++seq.current, at: Date.now() / 1000 });
    if (feedBuf.current.length > 60) feedBuf.current.splice(0, feedBuf.current.length - 60);
  }, []);

  // Resolve outstanding submissions by polling their status.
  const settle = useCallback(async (hash: string) => {
    const w = waiters.current.get(hash);
    if (!w) return;
    const rec = await api.tx(hash).catch(() => null);
    if (!rec || rec.status === 'pending') return;
    waiters.current.delete(hash);
    if (rec.status === 'confirmed') w.resolve(rec);
    else w.reject(new Error(rec.error ?? 'transaction rejected'));
  }, []);

  // Live feed: the gRPC-Web Watch stream (LedgerService.Watch) from whichever
  // chain node the ingress picks. Every event carries the cluster-wide sequence
  // number, so a reconnect resumes exactly where the last stream stopped.
  useEffect(() => {
    const onEvent = (ev: NodeEvent) => {
      for (const fn of listeners.current) fn(ev);
      if (ev.type === 'block') {
        setHeight(ev.height);
        setVersion((v) => v + 1);
        // Refresh only cached accounts this block touched.
        enqueue(ev.accounts.filter((a) => known.current.has(a)), true);
        const fails = ev.transitions.filter((t) => t.to === 'failed' || t.to === 'expired' || t.to === 'declined').length;
        pushFeed({
          kind: 'block',
          text: `Block #${ev.height}: ${ev.tx_count} tx, ${ev.transitions.length} deal moves${fails ? `, ${fails} failed/lapsed` : ''}${ev.rejected ? `, ${ev.rejected} rejected` : ''}`,
          link: `/explorer/block/${ev.height}`,
        });
        for (const t of ev.transitions.slice(0, 3)) {
          pushFeed({ kind: 'transition', text: `${t.id} ${t.from ? STATUS_LABEL[t.from] : 'new'} → ${STATUS_LABEL[t.to]}`, link: `/deals/${t.id}` });
        }
      } else if (ev.type === 'rejected') {
        pushFeed({ kind: 'rejected', text: `Tx ${ev.hash.slice(0, 10)}… rejected: ${ev.error}`, link: `/explorer/tx/${ev.hash}` });
      } else if (ev.type === 'refused') {
        pushFeed({ kind: 'refused', text: `Refused at admission: ${ev.error}` });
      } else if (ev.type === 'pending') {
        pushFeed({ kind: 'pending', text: `${ACTION_LABEL[ev.action] ?? ev.action} queued`, link: `/explorer/tx/${ev.hash}` });
      }
      if (ev.type === 'block' || ev.type === 'rejected') {
        for (const h of waiters.current.keys()) settle(h);
      }
    };

    let stopped = false;
    let ac = new AbortController();
    let lastMsg = Date.now();
    (async () => {
      let backoff = 500;
      while (!stopped) {
        ac = new AbortController();
        const s = stream.current;
        // After a long gap (sleep, network loss) skip the backlog and refetch instead.
        const resume = s.seq > 0n && Date.now() - lastMsg < 60_000;
        if (!resume) api.stats().then((st) => setHeight(st.height)).catch(() => {});
        try {
          const events = ledger.watch({ afterSeq: resume ? s.seq : 0n, withTransactions: true }, { signal: ac.signal });
          for await (const raw of events) {
            if (!s.up) {
              s.up = true;
              s.since = Date.now();
            }
            backoff = 500;
            lastMsg = Date.now();
            s.seq = raw.seq;
            s.events++;
            const ev = toNodeEvent(raw);
            if (ev) onEvent(ev);
          }
        } catch {
          /* aborted, node restarting, or network: reconnect below */
        }
        s.up = false;
        if (stopped) return;
        s.reconnects++;
        await new Promise((r) => setTimeout(r, backoff));
        backoff = Math.min(backoff * 2, 10_000);
      }
    })();
    // A proxy can keep a dead stream open after the node goes away, so restart
    // it if nothing has arrived for a while. Also settle submissions in case an
    // event was missed.
    const poll = setInterval(() => {
      for (const h of waiters.current.keys()) settle(h);
      if (stream.current.up && Date.now() - lastMsg > 20_000) ac.abort();
    }, 3000);
    return () => {
      stopped = true;
      ac.abort();
      clearInterval(poll);
    };
  }, [pushFeed, settle, enqueue]);

  const submit = useCallback(
    async (action: Action, opts?: { wallet?: LocalWallet }) => {
      const wallet = opts?.wallet ?? active;
      const label = ACTION_LABEL[action.type] ?? action.type;
      // Every transaction is made by a signed-in account that owns the wallet.
      const claims = await currentClaims();
      const precheck = !wallet
        ? 'Create or import a wallet first.'
        : !claims
          ? 'Sign in to make transactions.'
          : !claims.wallets.includes(wallet.address)
            ? 'Link this wallet to your account first (Wallet page → Link).'
            : null;
      if (precheck) {
        toast('err', `${label}: ${precheck}`);
        throw new Error(precheck);
      }
      setInflight((n) => n + 1);
      try {
        const signed = signAction(wallet!.secret, action);
        const confirmed = new Promise<TxRecord>((resolve, reject) => {
          waiters.current.set(signed.hash, { resolve, reject });
        });
        try {
          await api.submit(signed);
        } catch (e) {
          waiters.current.delete(signed.hash);
          throw e;
        }
        toast('info', `${label}: signed & broadcast, waiting for next block…`);
        const rec = await confirmed;
        toast('ok', `${label} confirmed in block #${rec.block_height}`);
        enqueue([wallet!.address], true);
        return rec;
      } catch (e) {
        const msg = e instanceof ApiError || e instanceof Error ? e.message : String(e);
        toast('err', `${label} failed: ${msg}`);
        throw e;
      } finally {
        setInflight((n) => n - 1);
      }
    },
    [active, toast, enqueue],
  );

  // Stable identities: pages subscribe in effects keyed on these.
  const subscribe = useCallback((fn: (e: NodeEvent) => void) => {
    listeners.current.add(fn);
    return () => {
      listeners.current.delete(fn);
    };
  }, []);
  const streamInfo = useCallback(() => stream.current, []);

  const value = useMemo<Ctx>(
    () => ({
      wallets,
      active,
      me: active ? directory.get(active.address) ?? null : null,
      setActive: setActiveAddr,
      addWallet: (w) => {
        setWallets((ws) => [...ws.filter((x) => x.address !== w.address), w]);
        setActiveAddr(w.address);
      },
      removeWallet: (addr) => setWallets((ws) => ws.filter((w) => w.address !== addr)),
      directory,
      resolve: (addrs) => enqueue(addrs),
      nameOf: (a) => {
        if (!a) return '—';
        const hit = directory.get(a);
        if (hit) return hit.name;
        enqueue([a]);
        return wallets.find((w) => w.address === a)?.label ?? `${a.slice(0, 8)}…`;
      },
      height,
      version,
      feed,
      toasts,
      toast,
      submit,
      busy: inflight > 0,
      subscribe,
      streamInfo,
    }),
    [wallets, active, directory, height, version, feed, toasts, toast, submit, inflight, enqueue, subscribe, streamInfo],
  );

  return <C.Provider value={value}>{children}</C.Provider>;
}
