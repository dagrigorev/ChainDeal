import type { Account, Block, ClusterInfo, ContractDocument, Deal, DocumentVerification, DealPolicy, Metrics, SignedTx, SimConfig, SimSnapshot, Stats, TxRecord, VerifyReport } from './types';

import { accessToken } from './auth';

/** Authorization header for protected chain-API calls (empty if signed out). */
async function bearer(): Promise<Record<string, string>> {
  const t = await accessToken();
  return t ? { authorization: `Bearer ${t}` } : {};
}

export class ApiError extends Error {
  constructor(message: string, public status: number) {
    super(message);
  }
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`/api${path}`, init);
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(body.error ?? res.statusText, res.status);
  return body as T;
}

// Lua returns empty tables as arrays; normalise to what callers expect.
const arr = <T,>(x: T[] | Record<string, never> | null | undefined): T[] => (Array.isArray(x) ? x : []);

export const api = {
  stats: () => req<Stats>('/stats'),
  policies: () => req<DealPolicy[]>('/policies'),
  blocks: async (before?: number, limit = 20) =>
    arr(await req<Block[]>(`/blocks?limit=${limit}${before != null ? `&before=${before}` : ''}`)),
  block: async (key: string | number) => {
    const r = await req<{ block: Block; txs: TxRecord[] }>(`/blocks/${key}`);
    return { block: r.block, txs: arr(r.txs) };
  },
  txs: async (limit = 30) => arr(await req<TxRecord[]>(`/txs?limit=${limit}`)),
  tx: (hash: string) => req<TxRecord>(`/tx/${hash}`),
  submit: async (tx: SignedTx) =>
    req<{ hash: string; status: string; deal_id: string | null }>('/tx', {
      method: 'POST',
      headers: { 'content-type': 'application/json', ...(await bearer()) },
      body: JSON.stringify(tx),
    }),
  /** Directory search, ordered by settled deals. */
  searchAccounts: async (q = '', kind = '', offset = 0, limit = 50) => {
    const r = await req<{ total: number; items: Account[] | Record<string, never> }>(
      `/accounts?q=${encodeURIComponent(q)}&kind=${kind}&offset=${offset}&limit=${limit}`,
    );
    return { total: r.total, items: arr(r.items) };
  },
  /** Batch resolve addresses to accounts (max 200 per call). */
  lookup: async (addrs: string[]) => (addrs.length ? arr(await req<Account[]>(`/accounts/lookup?addrs=${addrs.join(',')}`)) : []),
  account: async (addr: string) => {
    const r = await req<{ account: Account; deals: Deal[]; txs: TxRecord[] }>(`/accounts/${addr}`);
    return { account: r.account, deals: arr(r.deals), txs: arr(r.txs) };
  },
  deals: async (limit = 100, opts: { type?: string; status?: string; offset?: number } = {}) =>
    arr(await req<Deal[]>(`/deals?limit=${limit}&offset=${opts.offset ?? 0}&type=${opts.type ?? ''}&status=${opts.status ?? ''}`)),
  deal: (id: string) => req<Deal>(`/deals/${id}`),
  verifyRange: (from: number, count: number, prev: string | null) =>
    req<VerifyReport>(`/chain/verify?from=${from}&count=${count}${prev ? `&prev=${prev}` : ''}`),
  metrics: () => req<Metrics>('/metrics'),
  sim: () => req<SimSnapshot>('/sim'),
  setSim: async (patch: Partial<SimConfig>) =>
    req<SimSnapshot>('/sim', {
      method: 'POST',
      headers: { 'content-type': 'application/json', ...(await bearer()) },
      body: JSON.stringify(patch),
    }),
  cluster: () => req<ClusterInfo>('/cluster'),
  document: (dealId: string) => req<ContractDocument>(`/deals/${dealId}/document`),
  verifyDocument: (deal: string, hash: string, sig: string) =>
    req<DocumentVerification>(`/documents/verify?deal=${encodeURIComponent(deal)}&hash=${hash}&sig=${sig}`),
  attestation: () => req<{ alg: string; key_id: string; public_key: string; chain_id: string }>('/attestation'),
};
