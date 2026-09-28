// Mirrors crates/core/src/types.rs. Amounts are integer minor units (1 DEAL = 100).

export type PartyKind = 'consumer' | 'business';
export type DealType = 'C2C' | 'C2B' | 'B2C' | 'B2B';
export type DealStatus =
  | 'proposed' | 'accepted' | 'funded' | 'shipped'
  | 'completed' | 'disputed' | 'resolved' | 'cancelled'
  | 'declined' | 'expired' | 'failed';
export type Role = 'seller' | 'buyer';
export type TxStatus = 'pending' | 'confirmed' | 'rejected';

export interface LineItem { name: string; qty: number; unit_price: number }

export type Action =
  | { type: 'register'; name: string; kind: PartyKind }
  | { type: 'transfer'; to: string; amount: number; memo: string }
  | { type: 'create_deal'; role: Role; counterparty: string; title: string; description: string; items: LineItem[]; arbiter: string | null }
  | { type: 'accept_deal'; deal_id: string }
  | { type: 'fund_deal'; deal_id: string }
  | { type: 'mark_shipped'; deal_id: string; tracking: string }
  | { type: 'confirm_receipt'; deal_id: string; rating: number }
  | { type: 'claim_release'; deal_id: string }
  | { type: 'open_dispute'; deal_id: string; reason: string }
  | { type: 'resolve_dispute'; deal_id: string; buyer_refund_bps: number; note: string }
  | { type: 'cancel_deal'; deal_id: string; reason: string }
  | { type: 'decline_deal'; deal_id: string; reason: string }
  | { type: 'expire_deal'; deal_id: string };

export interface TxBody { from: string; pubkey: string; nonce: number; timestamp: number; action: Action }
export interface SignedTx { hash: string; body: TxBody; signature: string }
export interface TxRecord extends SignedTx {
  status: TxStatus;
  error: string | null;
  block_height: number | null;
  deal_id: string | null;
}

export interface Account {
  address: string;
  pubkey: string;
  name: string;
  kind: PartyKind;
  balance: number;
  escrowed: number;
  created_at: number;
  deals_completed: number;
  disputes: number;
  rating_sum: number;
  rating_count: number;
  /** Deadlines this account missed. */
  defaults: number;
}

export interface DealEvent { status: DealStatus; by: string; tx_hash: string; at: number; note: string }

export interface Deal {
  id: string;
  title: string;
  description: string;
  deal_type: DealType;
  seller: string;
  buyer: string;
  arbiter: string | null;
  proposer: string;
  items: LineItem[];
  amount: number;
  fee: number;
  bond: number;
  bond_locked: boolean;
  status: DealStatus;
  tracking: string | null;
  created_at: number;
  updated_at: number;
  shipped_at: number | null;
  release_after: number | null;
  buyer_refund_bps: number | null;
  dispute_reason: string | null;
  history: DealEvent[];
  /** When the current step lapses (accept-by / fund-by / deliver-by). */
  deadline: number | null;
}

export interface BlockHeader {
  height: number;
  prev_hash: string;
  timestamp: number;
  merkle_root: string;
  tx_count: number;
  difficulty: number;
  nonce: number;
  producer: string;
}
export interface Block { header: BlockHeader; hash: string; tx_hashes: string[] }

export interface DealPolicy {
  deal_type: DealType;
  fee_bps: number;
  seller_bond_bps: number;
  release_window_secs: number;
  arbiter_required: boolean;
  buyer_can_withdraw: boolean;
  accept_window_secs: number;
  fund_window_secs: number;
  delivery_window_secs: number;
  summary: string;
}

export interface Stats {
  height: number;
  tip_hash: string | null;
  tip_time: number;
  accounts: number;
  deals: number;
  txs: number;
  mempool: number;
  deals_by_status: Partial<Record<DealStatus, number>>;
  deals_by_type: Partial<Record<DealType, number>>;
  volume: number;
  settled_volume: number;
  treasury: number;
  difficulty: number;
  node: string;
}

export interface VerifyReport {
  valid: boolean;
  blocks_checked: number;
  txs_checked: number;
  errors: string[];
  elapsed_ms: number;
  next_from: number;
  last_hash: string | null;
  tip_height: number;
  done: boolean;
}

export interface Metrics {
  now: number;
  /** Per second, oldest first: [admitted, refused, confirmed, rejected]. */
  series: [number, number, number, number][];
  totals: { admitted: number; refused: number; confirmed: number; rejected: number };
}

export interface SimConfig { running: boolean; rate: number; pressure: number; noise: number }
export interface SimSnapshot {
  config: SimConfig;
  /** Node currently holding the producer lease. */
  leader?: string | null;
  /** Who may change the simulator ("operator" when authorization is on). */
  control_role?: string;
  agents: number;
  active: number;
  active_by_status: Record<string, number>;
  stats: {
    sent: number;
    admitted: number;
    refused: number;
    noise_sent: number;
    deals_opened: number;
    adopted: number;
    registered: number;
    by_action: Record<string, number>;
    scenarios: Record<string, number>;
    outcomes: Record<string, number>;
    recent_refusals: string[];
  };
}

export interface TarantoolInstance {
  id: number;
  uuid: string;
  ro: boolean;
  status: string;
  lsn: number;
  uptime: number;
  hostname: string;
  memory_used: number;
  memory_limit: number;
  txs: number;
  replication: { id: number; lsn: number; upstream: { status: string; lag: number } | null; downstream: { status: string; lag: number } | null }[];
}

export interface ClusterInfo {
  serving_node: string;
  leader: string | null;
  nodes: { id: string; age: number; info: string }[];
  master: TarantoolInstance;
  read_replica: TarantoolInstance | null;
}

export interface Transition {
  id: string;
  from: DealStatus | null;
  to: DealStatus;
  deal_type: DealType;
  amount: number;
  title: string;
}
