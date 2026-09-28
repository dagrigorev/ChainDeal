import { STATUS_LABEL } from '../lib/format';
import type { DealStatus, Stats } from '../lib/types';

const ORDER: DealStatus[] = ['completed', 'resolved', 'cancelled', 'declined', 'expired', 'failed'];
const OPEN: DealStatus[] = ['proposed', 'accepted', 'funded', 'shipped', 'disputed'];

/**
 * All-time outcome ledger: one proportional bar across every deal on chain,
 * split into settled and failed outcomes, with open deals shown hatched.
 */
export default function Outcomes({ stats }: { stats: Stats }) {
  const by = stats.deals_by_status ?? {};
  const open = OPEN.reduce((n, s) => n + (by[s] ?? 0), 0);
  const total = Math.max(1, stats.deals);
  const settled = (by.completed ?? 0) + (by.resolved ?? 0);
  const failed = (by.cancelled ?? 0) + (by.declined ?? 0) + (by.expired ?? 0) + (by.failed ?? 0);
  const pct = (n: number) => `${((100 * n) / total).toFixed(1)}%`;
  return (
    <figure className="outcomes">
      <div className="outcome-bar" role="img"
        aria-label={`${stats.deals.toLocaleString()} deals: ${pct(settled)} settled, ${pct(failed)} did not go through, ${pct(open)} still open`}>
        {ORDER.map((s) => (by[s] ?? 0) > 0 && (
          <span key={s} className={`ob ob-${s}`} style={{ flexGrow: by[s] }} title={`${STATUS_LABEL[s]}: ${(by[s] ?? 0).toLocaleString()} (${pct(by[s] ?? 0)})`} />
        ))}
        {open > 0 && <span className="ob ob-open" style={{ flexGrow: open }} title={`Open: ${open.toLocaleString()}`} />}
      </div>
      <figcaption className="outcome-legend">
        {ORDER.map((s) => (
          <span key={s}><i className={`ob-${s}`} aria-hidden />{STATUS_LABEL[s]} <b>{(by[s] ?? 0).toLocaleString()}</b> <span className="muted">{pct(by[s] ?? 0)}</span></span>
        ))}
        <span><i className="ob-open" aria-hidden />Open <b>{open.toLocaleString()}</b></span>
      </figcaption>
    </figure>
  );
}
