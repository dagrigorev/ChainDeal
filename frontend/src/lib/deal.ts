import type { Deal, DealPolicy } from './types';

export type Party = 'seller' | 'buyer' | 'arbiter' | null;

export function partyOf(d: Deal, addr: string | undefined): Party {
  if (!addr) return null;
  if (d.seller === addr) return 'seller';
  if (d.buyer === addr) return 'buyer';
  if (d.arbiter === addr) return 'arbiter';
  return null;
}

/** Mirrors the contract rules so the UI only offers actions that can succeed. */
export function availableActions(d: Deal, me: Party, isProposer: boolean, p: DealPolicy | undefined, now: number) {
  const lapsed = d.deadline != null && now >= d.deadline;
  const a = {
    decline: false,
    expire: false,
    expireIn: d.deadline != null ? Math.max(0, Math.ceil(d.deadline - now)) : null as number | null,
    accept: false,
    fund: false,
    ship: false,
    confirm: false,
    claim: false,
    claimIn: 0,
    dispute: false,
    resolve: false,
    cancel: false,
  };
  if (!me) return a;
  const party = me === 'seller' || me === 'buyer';
  // Once a deadline lapses the only way forward is to enforce it.
  a.expire = lapsed && (d.status === 'proposed' || d.status === 'accepted' || d.status === 'funded');
  switch (d.status) {
    case 'proposed':
      a.accept = party && !isProposer && !lapsed;
      a.decline = party && !isProposer;
      a.cancel = party && isProposer;
      break;
    case 'accepted':
      a.fund = me === 'buyer' && !lapsed;
      a.cancel = party;
      break;
    case 'funded':
      a.ship = me === 'seller' && !lapsed;
      a.cancel = me === 'seller' || (me === 'buyer' && !!p?.buyer_can_withdraw);
      a.dispute = party && !!d.arbiter;
      break;
    case 'shipped':
      a.confirm = me === 'buyer';
      a.claim = me === 'seller';
      a.claimIn = Math.max(0, Math.ceil((d.release_after ?? 0) - now));
      a.dispute = party && !!d.arbiter;
      break;
    case 'disputed':
      a.resolve = me === 'arbiter';
      break;
  }
  return a;
}

export type Spot = 'buyer' | 'custody' | 'seller';

/**
 * Where value physically sits for a deal — the backbone of the "Custody" UI.
 * Derived only from contract state, so the picture can never disagree with the chain.
 */
export function fundsPosition(d: Deal) {
  let principal: Spot | 'split';
  switch (d.status) {
    case 'funded':
    case 'shipped':
    case 'disputed':
      principal = 'custody';
      break;
    case 'completed':
      principal = 'seller';
      break;
    case 'resolved':
      principal = 'split';
      break;
    default:
      principal = 'buyer'; // proposed / accepted / never funded / refunded
  }
  const bond: Spot | 'none' =
    d.bond === 0 ? 'none'
    : d.bond_locked ? 'custody'
    : d.status === 'resolved' && (d.buyer_refund_bps ?? 0) > 5000 ? 'buyer'
    : d.status === 'failed' ? 'buyer' // bond is always locked once funded, and slashed on seller default
    : 'seller';
  const buyerShareBps = d.status === 'resolved' ? d.buyer_refund_bps ?? 0 : null;
  return {
    principal,
    bond,
    buyerShareBps,
    /** Value is only promised (not yet committed by the buyer). */
    promised: d.status === 'proposed',
    returned: d.status === 'cancelled' || d.status === 'declined' || d.status === 'expired' || d.status === 'failed',
    feeTaken: d.status === 'completed' || d.status === 'resolved',
  };
}

export function fundsSentence(d: Deal, fmt: (n: number) => string): string {
  const p = fundsPosition(d);
  const amt = `${fmt(d.amount)} DEAL`;
  const bond = p.bond === 'custody' ? ` The seller's ${fmt(d.bond)} bond is locked alongside it.` : '';
  switch (p.principal) {
    case 'custody':
      return `${amt} is locked in custody.${bond}`;
    case 'seller':
      return `${fmt(d.amount - d.fee)} DEAL was released to the seller; ${fmt(d.fee)} went to the treasury.`;
    case 'split': {
      const bps = p.buyerShareBps ?? 0;
      const refund = Math.floor((d.amount * bps) / 10000);
      const bondFate = d.bond > 0 ? ` The seller's bond was ${bps > 5000 ? 'forfeited to the buyer' : 'returned'}.` : '';
      return `The arbiter awarded ${(bps / 100).toFixed(2)}% to the buyer: ${fmt(refund)} to the buyer and ${fmt(d.amount - refund - d.fee)} to the seller.${bondFate}`;
    }
    default:
      if (d.status === 'failed')
        return `The seller missed the delivery deadline, so the deal failed. ${amt} was refunded to the buyer.${d.bond > 0 ? ` The seller's ${fmt(d.bond)} bond was forfeited to the buyer.` : ''}`;
      if (d.status === 'declined') return `The counterparty declined the proposal. No value moved.`;
      if (d.status === 'expired')
        return d.history.some((e) => e.status === 'accepted')
          ? `The buyer never funded custody before the deadline, so the deal expired as a buyer default.`
          : `Nobody accepted the proposal in time, so it lapsed. No value moved.`;
      if (p.returned) return `No value is held. The deal was cancelled and everything stayed with its owner.`;
      if (p.promised) return `${amt} is proposed. Nothing is locked until the buyer funds it.${bond}`;
      return `${amt} is agreed and waits with the buyer to be funded.${bond}`;
  }
}

/** Short hint of who has to act next, from the viewer's perspective. */
export function nextStep(d: Deal, me: Party, now = Date.now() / 1000): { text: string; mine: boolean } {
  const proposerIsSeller = d.proposer === d.seller;
  if (d.deadline != null && now >= d.deadline && (d.status === 'proposed' || d.status === 'accepted' || d.status === 'funded')) {
    return { text: 'Deadline passed — can be enforced', mine: me != null };
  }
  switch (d.status) {
    case 'proposed': {
      const waiting = proposerIsSeller ? 'buyer' : 'seller';
      return { text: `Waiting for ${waiting} to accept`, mine: me === waiting };
    }
    case 'accepted':
      return { text: 'Buyer to fund escrow', mine: me === 'buyer' };
    case 'funded':
      return { text: 'Seller to deliver', mine: me === 'seller' };
    case 'shipped':
      return { text: 'Buyer to confirm receipt', mine: me === 'buyer' };
    case 'disputed':
      return { text: 'Arbiter to rule', mine: me === 'arbiter' };
    case 'failed':
      return { text: 'Failed — seller defaulted', mine: false };
    case 'expired':
      return { text: 'Expired', mine: false };
    case 'declined':
      return { text: 'Declined', mine: false };
    default:
      return { text: 'Closed', mine: false };
  }
}
