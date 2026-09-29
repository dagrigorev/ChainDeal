// gRPC-Web client for chaindeal.v1.LedgerService (proto/chaindeal/v1/ledger.proto).
// The browser's live feed: node events and every confirmed transaction, streamed
// in protobuf through the ingress to whichever chain node serves the connection.
import { createClient } from '@connectrpc/connect';
import { createGrpcWebTransport } from '@connectrpc/connect-web';
import { LedgerService, type LedgerEvent, type Transaction } from '../gen/chaindeal/v1/ledger_pb.js';
import type { DealStatus, DealType, Transition } from './types';

export const ledger = createClient(LedgerService, createGrpcWebTransport({ baseUrl: location.origin }));

/** A confirmed transaction as streamed: the signed body plus summary fields. */
export interface StreamTx {
  hash: string;
  from: string;
  action: string;
  status: string;
  error: string;
  blockHeight: number;
  dealId: string;
  /** Canonical JSON covered by the signature: re-verifiable in the browser. */
  bodyJson: string;
  signature: string;
}

/** A node event, for pages that visualise the live stream. */
export type NodeEvent =
  | { type: 'block'; seq: bigint; height: number; hash: string; tx_count: number; rejected: number; transitions: Transition[]; accounts: string[]; transactions: StreamTx[] }
  | { type: 'pending'; seq: bigint; hash: string; action: string }
  | { type: 'refused'; seq: bigint; error: string }
  | { type: 'rejected'; seq: bigint; hash: string; from: string; error: string }
  | { type: 'leader'; seq: bigint; node: string; leader: boolean };

const tx = (t: Transaction): StreamTx => ({
  hash: t.hash,
  from: t.from,
  action: t.action,
  status: t.status,
  error: t.error,
  blockHeight: Number(t.blockHeight),
  dealId: t.dealId,
  bodyJson: t.bodyJson,
  signature: t.signature,
});

export function toNodeEvent(ev: LedgerEvent): NodeEvent | null {
  const seq = ev.seq;
  const e = ev.event;
  switch (e.case) {
    case 'pending':
      return { type: 'pending', seq, hash: e.value.hash, action: e.value.action };
    case 'refused':
      return { type: 'refused', seq, error: e.value.error };
    case 'rejected':
      return { type: 'rejected', seq, hash: e.value.hash, from: e.value.from, error: e.value.error };
    case 'leader':
      return { type: 'leader', seq, node: e.value.node, leader: e.value.leader };
    case 'block': {
      const b = e.value;
      return {
        type: 'block',
        seq,
        height: Number(b.height),
        hash: b.hash,
        tx_count: b.txCount,
        rejected: b.rejected,
        transitions: b.transitions.map((t) => ({
          id: t.id,
          from: (t.from || null) as DealStatus | null,
          to: t.to as DealStatus,
          deal_type: t.dealType as DealType,
          amount: Number(t.amount),
          title: t.title,
        })),
        accounts: b.accounts,
        transactions: b.transactions.map(tx),
      };
    }
    default:
      return null;
  }
}
