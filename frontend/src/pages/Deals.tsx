import { useState } from 'react';
import { MoneyTrack } from '../components/Bridge';
import { Addr, Amount, Empty, ErrorNote, Loading, StatusBadge, TypeBadge, useLive } from '../components/ui';
import { api } from '../lib/api';
import { nextStep, partyOf } from '../lib/deal';
import { isClosed, STATUS_LABEL, timeAgo } from '../lib/format';
import { href } from '../lib/router';
import { useStore } from '../lib/store';
import type { Deal, DealStatus, DealType } from '../lib/types';

const STATUS_FILTERS: ('all' | 'open' | DealStatus)[] = [
  'all', 'open', 'proposed', 'accepted', 'funded', 'shipped', 'disputed',
  'completed', 'resolved', 'cancelled', 'declined', 'expired', 'failed',
];

type Tab = 'action' | 'active' | 'closed' | 'market';

export default function Deals() {
  const { active } = useStore();
  const [tab, setTab] = useState<Tab>(active ? 'action' : 'market');
  const [type, setType] = useState<DealType | 'all'>('all');
  const [status, setStatus] = useState<'all' | 'open' | DealStatus>('all');
  const [limit, setLimit] = useState(100);
  const mine = useLive(() => (active ? api.account(active.address).then((r) => r.deals) : Promise.resolve([] as Deal[])), [active?.address]);
  // Network view is filtered server-side: there are hundreds of thousands of deals.
  const all = useLive(
    () => api.deals(limit, { type: type === 'all' ? '' : type, status: status === 'all' ? '' : status }),
    [limit, type, status],
  );

  const source = tab === 'market' ? all : mine;
  const me = active?.address;
  const rows = (source.data ?? [])
    .filter((d) => type === 'all' || d.deal_type === type)
    .filter((d) => {
      if (tab === 'action') return nextStep(d, partyOf(d, me)).mine;
      if (tab === 'active') return !isClosed(d.status);
      if (tab === 'closed') return isClosed(d.status);
      return true;
    });
  const count = (t: Tab) =>
    (mine.data ?? []).filter((d) =>
      t === 'action' ? nextStep(d, partyOf(d, me)).mine : t === 'active' ? !isClosed(d.status) : isClosed(d.status),
    ).length;

  return (
    <div className="page">
      <div className="page-h">
        <h1>Deals</h1>
        {active && <a className="btn primary" href={href('/deals/new')}>Propose a deal</a>}
      </div>

      <div className="toolbar">
        <div className="tabs" role="tablist" aria-label="Deal view">
          {active && (['action', 'active', 'closed'] as const).map((t) => (
            <button key={t} role="tab" aria-selected={tab === t} className={tab === t ? 'on' : ''} onClick={() => setTab(t)}>
              {t === 'action' ? 'Your move' : t === 'active' ? 'In progress' : 'Closed'}
              <span className="count">{count(t)}</span>
            </button>
          ))}
          <button role="tab" aria-selected={tab === 'market'} className={tab === 'market' ? 'on' : ''} onClick={() => setTab('market')}>All network deals</button>
        </div>
        <div className="row gap">
          {tab === 'market' && (
            <>
              <label className="sr-only" htmlFor="st-filter">Status</label>
              <select id="st-filter" className="compact" value={status} onChange={(e) => { setStatus(e.target.value as typeof status); setLimit(100); }}>
                {STATUS_FILTERS.map((f) => <option key={f} value={f}>{f === 'all' ? 'Any status' : f === 'open' ? 'Open (in progress)' : STATUS_LABEL[f]}</option>)}
              </select>
            </>
          )}
          <div className="seg small" role="group" aria-label="Filter by type">
            {(['all', 'C2C', 'C2B', 'B2C', 'B2B'] as const).map((t) => (
              <button key={t} aria-pressed={type === t} className={type === t ? 'on' : ''} onClick={() => { setType(t); setLimit(100); }}>{t === 'all' ? 'All types' : t}</button>
            ))}
          </div>
        </div>
      </div>

      {source.error && <ErrorNote msg={source.error} />}
      {source.loading ? <Loading /> : rows.length === 0 ? (
        <div className="sheet"><Empty>No deals here.{active && tab !== 'market' && <> <a href={href('/deals/new')}>Propose one →</a></>}</Empty></div>
      ) : (
        <>
          <div className="docket-legend" aria-hidden>
            <span>Deal</span><span title="Buyer · custody · seller">Funds</span><span>Next</span><span className="r">Amount</span>
          </div>
          <ol className="docket">
            {rows.map((d) => <DocketRow key={d.id} d={d} me={me} />)}
          </ol>
          {tab === 'market' && rows.length >= limit && (
            <button className="btn block" onClick={() => setLimit(limit + 100)}>Load 100 more</button>
          )}
        </>
      )}
    </div>
  );
}

export function DocketRow({ d, me }: { d: Deal; me?: string }) {
  const party = partyOf(d, me);
  const step = nextStep(d, party);
  const other = party === 'seller' ? d.buyer : party === 'buyer' ? d.seller : null;
  return (
    <li className={`docket-row ${step.mine ? 'mine' : ''}`}>
      <a href={href(`/deals/${d.id}`)}>
        <span className="dk-main">
          <span className="dk-title">{d.title}</span>
          <span className="dk-meta">
            <TypeBadge type={d.deal_type} />
            {other ? <>with <Addr a={other} link={false} /></> : <><Addr a={d.seller} link={false} /> → <Addr a={d.buyer} link={false} /></>}
            {party && <span className={`role-chip role-${party}`}>{party}</span>}
          </span>
        </span>
        <MoneyTrack d={d} />
        <span className="dk-next">
          <StatusBadge status={d.status} />
          <span className={`step ${step.mine ? 'mine' : ''}`}>{step.text}</span>
        </span>
        <span className="dk-amt">
          <Amount v={d.amount} className="big" />
          <span className="muted tiny">{timeAgo(d.updated_at)}</span>
        </span>
      </a>
    </li>
  );
}
