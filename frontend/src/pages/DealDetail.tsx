import { useState } from 'react';
import Bridge from '../components/Bridge';
import { Addr, Amount, Card, ErrorNote, Hash, Loading, StatusBadge, TypeBadge, useLive, useNow } from '../components/ui';
import { api } from '../lib/api';
import { availableActions, fundsSentence, nextStep, partyOf } from '../lib/deal';
import { fmtAmount, fmtDuration, fmtTime, STATUS_LABEL, timeAgo } from '../lib/format';
import { href } from '../lib/router';
import { useStore } from '../lib/store';
import type { Deal, DealStatus } from '../lib/types';
import { policies } from '../lib/wasm';

const HAPPY: DealStatus[] = ['proposed', 'accepted', 'funded', 'shipped', 'completed'];

export default function DealDetail({ id }: { id: string }) {
  const deal = useLive(() => api.deal(id), [id]);
  if (deal.error) return <div className="page"><ErrorNote msg={deal.error === 'deal not found' ? 'Deal not found yet — it appears once its block is sealed.' : deal.error} /></div>;
  if (!deal.data) return <div className="page"><Loading /></div>;
  return <DealView d={deal.data} />;
}

function DealView({ d }: { d: Deal }) {
  const { active, busy } = useStore();
  const now = useNow();
  const policy = policies().find((p) => p.deal_type === d.deal_type)!;
  const me = partyOf(d, active?.address);
  const step = nextStep(d, me, now);
  const acts = availableActions(d, me, d.proposer === active?.address, policy, now);
  const hasActions = acts.decline || acts.expire || acts.accept || acts.fund || acts.ship || acts.confirm || acts.claim || acts.dispute || acts.resolve || acts.cancel;
  // Arbiter's split slider, lifted here so the bridge can preview the ruling live.
  const [share, setShare] = useState(50);

  return (
    <div className="page deal-page">
      <header className="deal-head">
        <div className="crumbs"><a href={href('/deals')}>Deals</a> / <code>{d.id}</code></div>
        <div className="deal-head-row">
          <div className="grow">
            <h1>{d.title}</h1>
            <div className="row gap">
              <TypeBadge type={d.deal_type} />
              <StatusBadge status={d.status} />
              <span className="muted small">opened {timeAgo(d.created_at)} · updated {timeAgo(d.updated_at)}</span>
            </div>
          </div>
          <div className="head-amount">
            <Amount v={d.amount} className="xl" />
            <div className={`step ${step.mine ? 'mine' : ''}`}>{step.mine ? 'Your move — ' : ''}{step.text}</div>
          </div>
        </div>
      </header>

      <Bridge d={d} policy={policy} pending={busy && !!me} previewBps={acts.resolve ? share * 100 : null} />

      <p className="funds-line">
        {fundsSentence(d, fmtAmount)}
        {d.status === 'shipped' && d.release_after && (
          now < d.release_after
            ? <> The buyer has <b>{fmtDuration(Math.ceil(d.release_after - now))}</b> to inspect before the seller may claim.</>
            : <> The inspection window has closed.</>
        )}
        {d.tracking && <> Delivery ref <code>{d.tracking}</code>.</>}
        {d.deadline != null && (
          now < d.deadline
            ? <> <span className="deadline">{d.status === 'proposed' ? 'Proposal lapses' : d.status === 'accepted' ? 'Buyer must fund' : 'Seller must deliver'} within <b>{fmtDuration(Math.ceil(d.deadline - now))}</b>.</span></>
            : <> <span className="deadline late">The {d.status === 'proposed' ? 'acceptance' : d.status === 'accepted' ? 'funding' : 'delivery'} deadline has passed{d.status === 'funded' ? ' — enforcing it fails the deal and refunds the buyer' : ''}.</span></>
        )}
      </p>

      <Rail d={d} />

      {me && hasActions && <Actions d={d} acts={acts} role={me} share={share} setShare={setShare} />}
      {d.status === 'disputed' && <div className="note err"><b>Dispute:</b> {d.dispute_reason} — awaiting a ruling from <Addr a={d.arbiter} />.</div>}

      <div className="grid-main">
        <div className="stack">
          <Card title="Terms">
            {d.description && <p className="desc">{d.description}</p>}
            <table className="table">
              <thead><tr><th scope="col">Item</th><th scope="col" className="r">Qty</th><th scope="col" className="r">Unit price</th><th scope="col" className="r">Total</th></tr></thead>
              <tbody>
                {d.items.map((it, i) => (
                  <tr key={i}><td>{it.name}</td><td className="r">{it.qty}</td><td className="r mono">{fmtAmount(it.unit_price)}</td><td className="r mono">{fmtAmount(it.qty * it.unit_price)}</td></tr>
                ))}
              </tbody>
              <tfoot>
                <tr><td colSpan={3}>Custody amount</td><td className="r"><Amount v={d.amount} /></td></tr>
                <tr className="muted"><td colSpan={3}>Protocol fee{d.status === 'resolved' ? ' (on seller share)' : ` (${policy.fee_bps / 100}%)`}</td><td className="r mono">−{fmtAmount(d.fee)}</td></tr>
                {d.bond > 0 && <tr className="muted"><td colSpan={3}>Seller performance bond {d.bond_locked ? '(locked)' : ''}</td><td className="r mono">{fmtAmount(d.bond)}</td></tr>}
              </tfoot>
            </table>
          </Card>

          <Card title="Ledger">
            <ol className="ledger">
              {d.history.map((e, i) => (
                <li key={i} className={`lg-${e.status}`}>
                  <span className="lg-when mono">{fmtTime(e.at)}</span>
                  <span className="lg-what"><StatusBadge status={e.status} /> <span className="lg-note">{e.note}</span></span>
                  <span className="lg-who"><Addr a={e.by} /> <Hash h={e.tx_hash} to={`/explorer/tx/${e.tx_hash}`} n={5} /></span>
                </li>
              ))}
            </ol>
          </Card>
        </div>

        <aside className="stack" aria-label="Contract">
          <Card title={<>{d.deal_type} contract</>}>
            <dl className="kv">
              <dt>Seller</dt><dd><Addr a={d.seller} /></dd>
              <dt>Buyer</dt><dd><Addr a={d.buyer} /></dd>
              <dt>Arbiter</dt><dd>{d.arbiter ? <Addr a={d.arbiter} /> : <span className="muted">none — disputes disabled</span>}</dd>
              <dt>Proposed by</dt><dd>{d.proposer === d.seller ? 'Seller' : 'Buyer'}</dd>
            </dl>
            {me && <p className={`role-line role-${me}`}>You are the {me}.</p>}
            <hr />
            <p className="muted small">{policy.summary}</p>
            <dl className="kv">
              <dt>Fee</dt><dd>{policy.fee_bps / 100}% of seller payout</dd>
              <dt>Seller bond</dt><dd>{policy.seller_bond_bps ? `${policy.seller_bond_bps / 100}%` : 'none'}</dd>
              <dt>Inspection window</dt><dd>{fmtDuration(policy.release_window_secs)}</dd>
              <dt>Arbiter</dt><dd>{policy.arbiter_required ? 'required' : 'optional'}</dd>
              <dt>Buyer withdrawal</dt><dd>{policy.buyer_can_withdraw ? 'until delivery' : 'not after funding'}</dd>
            </dl>
          </Card>
        </aside>
      </div>
    </div>
  );
}

/** Thin lifecycle rail under the bridge (the bridge shows *where*, the rail shows *when*). */
function Rail({ d }: { d: Deal }) {
  const reached = new Set(d.history.map((e) => e.status));
  const branch = ['cancelled', 'disputed', 'resolved', 'declined', 'expired', 'failed'].includes(d.status);
  const steps: DealStatus[] = branch
    ? [...HAPPY.filter((s) => reached.has(s)), ...(d.status === 'resolved' ? (['disputed', 'resolved'] as DealStatus[]) : [d.status])]
    : HAPPY;
  const cur = steps.indexOf(d.status);
  return (
    <ol className="rail" aria-label="Deal lifecycle">
      {steps.map((s, i) => (
        <li key={`${s}-${i}`} className={`${i < cur ? 'done' : ''} ${i === cur ? 'cur' : ''} rail-${s}`} aria-current={i === cur ? 'step' : undefined}>
          {STATUS_LABEL[s]}
        </li>
      ))}
    </ol>
  );
}

interface ActionsProps {
  d: Deal;
  acts: ReturnType<typeof availableActions>;
  role: string;
  share: number;
  setShare(n: number): void;
}

function Actions({ d, acts, role, share, setShare }: ActionsProps) {
  const { submit, busy } = useStore();
  const [tracking, setTracking] = useState('');
  const [rating, setRating] = useState(5);
  const [reason, setReason] = useState('');
  const [note, setNote] = useState('');
  const [open, setOpen] = useState<'dispute' | 'cancel' | null>(null);
  const run = (a: Parameters<typeof submit>[0]) => submit(a).catch(() => {});
  const id = d.id;
  const buyerCut = Math.floor((d.amount * share) / 100);

  return (
    <section className="slip" aria-labelledby="slip-h">
      <h2 id="slip-h" className="slip-h">Your move</h2>
      {acts.expire && (
        <div className="act">
          <p>
            {d.status === 'funded'
              ? <>The seller missed the delivery deadline. Enforcing it <b>fails the deal</b>: the buyer is refunded{d.bond > 0 ? ' and the seller bond is forfeited to the buyer' : ''}, and the seller records a default.</>
              : d.status === 'accepted'
                ? <>The buyer never funded custody in time. Enforcing the deadline expires the deal and records a buyer default.</>
                : <>Nobody accepted this proposal in time. Enforcing the deadline closes it as expired.</>}
          </p>
          <button className="btn primary" disabled={busy} onClick={() => run({ type: 'expire_deal', deal_id: id })}>Enforce deadline</button>
        </div>
      )}
      {acts.accept && (
        <div className="act">
          <p>
            Review the terms. {role === 'seller' && d.bond > 0 && <>Accepting locks your <b>{fmtAmount(d.bond)} DEAL</b> performance bond in custody. </>}
            {acts.expireIn != null && <>The offer lapses in <b>{fmtDuration(acts.expireIn)}</b>.</>}
          </p>
          <div className="row gap">
            <button className="btn primary" disabled={busy} onClick={() => run({ type: 'accept_deal', deal_id: id })}>Accept terms</button>
            <button className="btn ghost" disabled={busy} onClick={() => run({ type: 'decline_deal', deal_id: id, reason: 'declined' })}>Decline</button>
          </div>
        </div>
      )}
      {acts.decline && !acts.accept && (
        <div className="act">
          <p>This proposal has lapsed, but you can still formally decline it.</p>
          <button className="btn ghost" disabled={busy} onClick={() => run({ type: 'decline_deal', deal_id: id, reason: 'declined' })}>Decline</button>
        </div>
      )}
      {acts.fund && (
        <div className="act">
          <p>Move <b>{fmtAmount(d.amount)} DEAL</b> into custody. The seller can't touch it until you confirm or the inspection window passes.</p>
          <button className="btn primary" disabled={busy} onClick={() => run({ type: 'fund_deal', deal_id: id })}>Fund custody</button>
        </div>
      )}
      {acts.ship && (
        <div className="act">
          <p>Mark as delivered. This starts the buyer's inspection window.</p>
          <div className="row gap">
            <label className="sr-only" htmlFor="trk">Tracking or delivery reference</label>
            <input id="trk" value={tracking} onChange={(e) => setTracking(e.target.value)} placeholder="Tracking / delivery reference (optional)" className="grow" />
            <button className="btn primary" disabled={busy} onClick={() => run({ type: 'mark_shipped', deal_id: id, tracking })}>Mark delivered</button>
          </div>
        </div>
      )}
      {acts.confirm && (
        <div className="act">
          <p>Got everything as agreed? Confirming releases custody to the seller.</p>
          <div className="row gap">
            <div className="stars" role="radiogroup" aria-label="Rate the seller">
              {[1, 2, 3, 4, 5].map((n) => (
                <button key={n} role="radio" aria-checked={n === rating} className={n <= rating ? 'on' : ''} onClick={() => setRating(n)} aria-label={`${n} star${n > 1 ? 's' : ''}`}>★</button>
              ))}
            </div>
            <button className="btn primary" disabled={busy} onClick={() => run({ type: 'confirm_receipt', deal_id: id, rating })}>Confirm & release</button>
          </div>
        </div>
      )}
      {acts.claim && (
        <div className="act">
          <p>{acts.claimIn > 0 ? <>You can claim payment in <b>{fmtDuration(acts.claimIn)}</b> if the buyer doesn't respond.</> : 'The inspection window passed with no dispute.'}</p>
          <button className="btn primary" disabled={busy || acts.claimIn > 0} onClick={() => run({ type: 'claim_release', deal_id: id })}>Claim payment</button>
        </div>
      )}
      {acts.resolve && (
        <div className="act">
          <p>
            Divide the <b>{fmtAmount(d.amount)} DEAL</b> in custody. The bridge above previews your ruling.
            {d.bond > 0 && ' Giving the buyer more than 50% also moves the seller bond to the buyer.'}
          </p>
          <div className="split">
            <span>Buyer <b>{share}%</b> · {fmtAmount(buyerCut)}</span>
            <input type="range" min={0} max={100} value={share} onChange={(e) => setShare(+e.target.value)}
              aria-label="Buyer share of custody" aria-valuetext={`${share}% to buyer, ${100 - share}% to seller`} />
            <span>Seller <b>{100 - share}%</b> · {fmtAmount(d.amount - buyerCut)}</span>
          </div>
          <div className="row gap">
            <label className="sr-only" htmlFor="ruling">Ruling note</label>
            <input id="ruling" value={note} onChange={(e) => setNote(e.target.value)} placeholder="Ruling note" className="grow" />
            <button className="btn primary" disabled={busy} onClick={() => run({ type: 'resolve_dispute', deal_id: id, buyer_refund_bps: share * 100, note })}>Issue ruling</button>
          </div>
        </div>
      )}
      {(acts.dispute || acts.cancel) && (
        <div className="act secondary">
          <div className="row gap">
            {acts.dispute && <button className="btn ghost danger" aria-expanded={open === 'dispute'} onClick={() => setOpen(open === 'dispute' ? null : 'dispute')}>Open dispute…</button>}
            {acts.cancel && <button className="btn ghost" aria-expanded={open === 'cancel'} onClick={() => setOpen(open === 'cancel' ? null : 'cancel')}>{d.status === 'funded' ? 'Cancel & refund buyer…' : 'Cancel deal…'}</button>}
          </div>
          {open && (
            <div className="row gap">
              <label className="sr-only" htmlFor="why">{open === 'dispute' ? 'Dispute reason' : 'Cancellation reason'}</label>
              <input id="why" value={reason} onChange={(e) => setReason(e.target.value)} placeholder={open === 'dispute' ? 'What went wrong?' : 'Reason (optional)'} className="grow" />
              <button
                className="btn danger"
                disabled={busy || (open === 'dispute' && !reason.trim())}
                onClick={() => run(open === 'dispute' ? { type: 'open_dispute', deal_id: id, reason } : { type: 'cancel_deal', deal_id: id, reason }).then(() => setOpen(null))}
              >
                {open === 'dispute' ? 'Submit dispute' : 'Confirm cancel'}
              </button>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
