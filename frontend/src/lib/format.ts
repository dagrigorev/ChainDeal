import type { DealStatus } from './types';

const nf = new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

export const fmtAmount = (minor: number) => nf.format(minor / 100);

/** Parses a user-entered decimal ("1,234.5") into minor units, or NaN. */
export function parseAmount(s: string): number {
  const clean = s.replace(/[,\s]/g, '');
  if (!/^\d+(\.\d{0,2})?$/.test(clean)) return NaN;
  return Math.round(parseFloat(clean) * 100);
}

export const short = (h: string | null | undefined, n = 6) =>
  !h ? '—' : h.length <= n * 2 + 1 ? h : `${h.slice(0, n + (h.startsWith('0x') ? 2 : 0))}…${h.slice(-4)}`;

export function timeAgo(secs: number): string {
  const d = Math.max(0, Math.floor(Date.now() / 1000 - secs));
  if (d < 5) return 'just now';
  if (d < 60) return `${d}s ago`;
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  return new Date(secs * 1000).toLocaleDateString();
}

export const fmtTime = (secs: number) => new Date(secs * 1000).toLocaleString();

export function fmtDuration(secs: number): string {
  if (secs <= 0) return '0s';
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  if (m && !s) return `${m}m`; // round policy windows read "3m", not "3m 00s"
  return m ? `${m}m ${s.toString().padStart(2, '0')}s` : `${s}s`;
}

export const STATUS_LABEL: Record<DealStatus, string> = {
  proposed: 'Proposed',
  accepted: 'Accepted',
  funded: 'Funded',
  shipped: 'Delivered',
  completed: 'Completed',
  disputed: 'Disputed',
  resolved: 'Resolved',
  cancelled: 'Cancelled',
  declined: 'Declined',
  expired: 'Expired',
  failed: 'Failed',
};

export const ACTION_LABEL: Record<string, string> = {
  register: 'Register',
  transfer: 'Transfer',
  create_deal: 'Create deal',
  accept_deal: 'Accept deal',
  fund_deal: 'Fund escrow',
  mark_shipped: 'Mark delivered',
  confirm_receipt: 'Confirm receipt',
  claim_release: 'Claim release',
  open_dispute: 'Open dispute',
  resolve_dispute: 'Resolve dispute',
  cancel_deal: 'Cancel deal',
  decline_deal: 'Decline deal',
  expire_deal: 'Enforce deadline',
};

/** Terminal states that mean the deal did not go through. */
export const FAILURE_STATES = ['declined', 'expired', 'failed', 'cancelled'] as const;

export const isClosed = (s: DealStatus) =>
  s === 'completed' || s === 'resolved' || s === 'cancelled' || s === 'declined' || s === 'expired' || s === 'failed';
