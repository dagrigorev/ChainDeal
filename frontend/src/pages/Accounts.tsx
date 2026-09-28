import { useEffect, useState } from 'react';
import { Addr, Amount, Avatar, Card, Copy, Empty, ErrorNote, Hash, KindTag, Loading, StatusBadge, TypeBadge, useLive } from '../components/ui';
import { api } from '../lib/api';
import { ACTION_LABEL, fmtAmount, fmtTime, timeAgo } from '../lib/format';
import { href } from '../lib/router';
import type { Account, PartyKind } from '../lib/types';

const rating = (a: Account) => (a.rating_count ? (a.rating_sum / a.rating_count).toFixed(1) : '—');

const PAGE = 50;

export default function Accounts() {
  const [kind, setKind] = useState<PartyKind | 'all'>('all');
  const [q, setQ] = useState('');
  const [debounced, setDebounced] = useState('');
  const [page, setPage] = useState(0);
  useEffect(() => {
    const t = setTimeout(() => { setDebounced(q.trim()); setPage(0); }, 200);
    return () => clearTimeout(t);
  }, [q]);
  const res = useLive(() => api.searchAccounts(debounced, kind === 'all' ? '' : kind, page * PAGE, PAGE), [debounced, kind, page]);
  const list = res.data?.items ?? [];
  const total = res.data?.total ?? 0;

  return (
    <div className="page">
      <div className="page-h">
        <h1>Directory</h1>
        <div className="seg small" role="group" aria-label="Filter by kind">
          {(['all', 'consumer', 'business'] as const).map((k) => (
            <button key={k} aria-pressed={kind === k} className={kind === k ? 'on' : ''} onClick={() => { setKind(k); setPage(0); }}>
              {k === 'all' ? 'Everyone' : k === 'consumer' ? 'Individuals' : 'Businesses'}
            </button>
          ))}
        </div>
      </div>
      <p className="lead">Every registered participant with their on-chain reputation: settled deals, seller rating, disputes and missed deadlines.</p>
      <div className="row gap">
        <label className="sr-only" htmlFor="dir-q">Search participants</label>
        <input id="dir-q" className="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search by name or address…" />
        <span className="muted small">{total.toLocaleString()} participants</span>
      </div>
      <Card>
        {res.loading ? <Loading /> : list.length === 0 ? <Empty>No accounts match.</Empty> : (
          <table className="table">
            <thead><tr><th scope="col">Participant</th><th scope="col">Address</th><th scope="col" className="r">Balance</th><th scope="col" className="r">In escrow</th><th scope="col" className="r">Deals</th><th scope="col" className="r">Rating</th><th scope="col" className="r">Disputes</th><th scope="col" className="r">Defaults</th></tr></thead>
            <tbody>
              {list.map((a) => (
                <tr key={a.address} className="clickable" onClick={() => (location.hash = `/accounts/${a.address}`)}>
                  <td><Addr a={a.address} /> <KindTag kind={a.kind} /></td>
                  <td><Hash h={a.address} /></td>
                  <td className="r"><Amount v={a.balance} unit={false} /></td>
                  <td className="r mono">{fmtAmount(a.escrowed)}</td>
                  <td className="r">{a.deals_completed}</td>
                  <td className="r">{rating(a)}{a.rating_count > 0 && <span className="muted small"> ({a.rating_count})</span>}</td>
                  <td className="r">{a.disputes}</td>
                  <td className={`r ${a.defaults ? 'text-seal' : ''}`}>{a.defaults ?? 0}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {total > PAGE && (
          <div className="pager">
            <button className="btn xs" disabled={page === 0} onClick={() => setPage(page - 1)}>← Previous</button>
            <span className="muted small">Page {page + 1} of {Math.ceil(total / PAGE)}</span>
            <button className="btn xs" disabled={(page + 1) * PAGE >= total} onClick={() => setPage(page + 1)}>Next →</button>
          </div>
        )}
      </Card>
    </div>
  );
}

export function AccountView({ addr }: { addr: string }) {
  const r = useLive(() => api.account(addr), [addr]);
  if (r.error) return <div className="page"><ErrorNote msg={r.error} /></div>;
  if (!r.data) return <div className="page"><Loading /></div>;
  const { account: a, deals, txs } = r.data;
  return (
    <div className="page">
      <div className="crumbs"><a href={href('/accounts')}>Directory</a> / account</div>
      <div className="profile">
        <Avatar a={a.address} size={56} />
        <div className="grow">
          <h1>{a.name} <KindTag kind={a.kind} /></h1>
          <div className="row gap"><code className="hash">{a.address}</code> <Copy text={a.address} /></div>
          <div className="muted small">member since {fmtTime(a.created_at)}</div>
        </div>
        <div className="profile-stats">
          <div><b><Amount v={a.balance} /></b><span>available</span></div>
          <div><b>{fmtAmount(a.escrowed)}</b><span>in escrow</span></div>
          <div><b>{a.deals_completed}</b><span>deals done</span></div>
          <div><b>{rating(a)}★</b><span>seller rating</span></div>
          <div><b>{a.disputes}</b><span>disputes</span></div>
          <div><b className={a.defaults ? 'text-seal' : ''}>{a.defaults ?? 0}</b><span>missed deadlines</span></div>
        </div>
      </div>
      <div className="grid-2">
        <Card title={deals.length >= 100 ? 'Latest 100 deals' : `Deals (${deals.length})`}>
          {deals.length === 0 ? <Empty>No deals yet.</Empty> : (
            <ul className="list">
              {deals.map((d) => (
                <li key={d.id}>
                  <a className="row-link" href={href(`/deals/${d.id}`)}>
                    <TypeBadge type={d.deal_type} />
                    <span className="grow ellipsis">{d.title}</span>
                    <span className="muted small">{d.seller === a.address ? 'seller' : d.buyer === a.address ? 'buyer' : 'arbiter'}</span>
                    <StatusBadge status={d.status} />
                    <Amount v={d.amount} unit={false} />
                  </a>
                </li>
              ))}
            </ul>
          )}
        </Card>
        <Card title="Transactions sent">
          {txs.length === 0 ? <Empty>None.</Empty> : (
            <ul className="list">
              {txs.map((t) => (
                <li key={t.hash}>
                  <a className="row-link" href={href(`/explorer/tx/${t.hash}`)}>
                    <Hash h={t.hash} n={6} />
                    <span className="grow">{ACTION_LABEL[t.body.action.type]}</span>
                    {t.body.action.type === 'transfer' && <span className="small">→ <Addr a={t.body.action.to} link={false} /></span>}
                    <span className={`tag ${t.status === 'confirmed' ? 'ok' : 'err'}`}>{t.status === 'confirmed' ? `#${t.block_height}` : 'rejected'}</span>
                    <span className="muted small">{timeAgo(t.body.timestamp / 1000)}</span>
                  </a>
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>
    </div>
  );
}
