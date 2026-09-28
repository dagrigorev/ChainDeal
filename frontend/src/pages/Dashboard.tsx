import { MoneyTrack } from '../components/Bridge';
import Outcomes from '../components/Outcomes';
import { Addr, Amount, Card, Empty, Hash, KindTag, Loading, StatusBadge, TypeBadge, useLive } from '../components/ui';
import { api } from '../lib/api';
import { fundsSentence, nextStep, partyOf } from '../lib/deal';
import { fmtAmount, timeAgo } from '../lib/format';
import { href } from '../lib/router';
import { useStore } from '../lib/store';
import type { DealType } from '../lib/types';
import { policies } from '../lib/wasm';
import { DocketRow } from './Deals';

const TYPES: DealType[] = ['C2C', 'C2B', 'B2C', 'B2B'];

export default function Dashboard() {
  const { active, me, feed } = useStore();
  const stats = useLive(api.stats);
  const blocks = useLive(() => api.blocks(undefined, 6));
  const mine = useLive(() => (active ? api.account(active.address) : Promise.resolve(null)), [active?.address]);
  const s = stats.data;

  const deals = mine.data?.deals ?? [];
  const actionable = deals.filter((d) => nextStep(d, partyOf(d, active?.address)).mine);
  const [first, ...rest] = actionable;
  const maxType = Math.max(1, ...TYPES.map((t) => s?.deals_by_type?.[t] ?? 0));

  return (
    <div className="page desk">
      {/* 1 — who is at the counter, and the one thing to do next */}
      {me ? (
        <section className="counter" aria-label="Your account">
          <div className="counter-id">
            <span className="eyebrow">At the counter</span>
            <h1>{me.name} <KindTag kind={me.kind} /></h1>
            <p className="counter-bal">
              <Amount v={me.balance} className="xl" />
              <span className="muted"> available · {fmtAmount(me.escrowed)} in custody · {me.deals_completed} {me.deals_completed === 1 ? 'deal' : 'deals'} settled</span>
            </p>
          </div>
          {!first && <a className="btn primary lg" href={href('/deals/new')}>Propose a deal</a>}
        </section>
      ) : (
        <section className="counter" aria-label="Get started">
          <div className="counter-id">
            <span className="eyebrow">Custody for every kind of deal</span>
            <h1>Agree the terms. Lock the value. Release it together.</h1>
            <p className="muted">
              Individuals and businesses place value in on-chain custody, and it only crosses when both sides are satisfied or an arbiter rules.
              Your keys are created and used in this browser by Rust compiled to WebAssembly.
            </p>
          </div>
          <a className="btn primary lg" href={href('/wallet')}>Create wallet</a>
        </section>
      )}

      {active && (
        <section aria-labelledby="move-h">
          <h2 id="move-h" className="section-h">Your move {actionable.length > 0 && <span className="count">{actionable.length}</span>}</h2>
          {mine.loading ? <Loading /> : !first ? (
            <div className="sheet"><Empty>Nothing is waiting on you. <a href={href('/deals')}>See all deals →</a></Empty></div>
          ) : (
            <>
              <a className="next-deal" href={href(`/deals/${first.id}`)}>
                <span className="nd-top">
                  <TypeBadge type={first.deal_type} />
                  <span className="nd-step">{nextStep(first, partyOf(first, active.address)).text}</span>
                </span>
                <span className="nd-title">{first.title}</span>
                <span className="nd-funds">
                  <MoneyTrack d={first} />
                  <span className="muted">{fundsSentence(first, fmtAmount)}</span>
                </span>
                <span className="nd-foot">
                  <Amount v={first.amount} className="big" />
                  <span className="btn primary">Open deal →</span>
                </span>
              </a>
              {rest.length > 0 && <ol className="docket">{rest.map((d) => <DocketRow key={d.id} d={d} me={active.address} />)}</ol>}
            </>
          )}
        </section>
      )}

      {/* 2 — network state as one typographic ledger line */}
      {!s ? <Loading /> : (
        <dl className="ledger-line" aria-label="Network">
          <div><dt>Height</dt><dd>#{s.height}</dd><dd className="sub">{timeAgo(s.tip_time)} · difficulty {s.difficulty}</dd></div>
          <div><dt>Transactions</dt><dd>{s.txs.toLocaleString()}</dd><dd className="sub">{s.mempool} in mempool</dd></div>
          <div><dt>Participants</dt><dd>{s.accounts - 1}</dd></div>
          <div><dt>Deals</dt><dd>{s.deals}</dd><dd className="sub">{s.deals_by_status.disputed ?? 0} disputed</dd></div>
          <div><dt>Settled</dt><dd><Amount v={s.settled_volume} /></dd><dd className="sub">of <Amount v={s.volume} /> contracted</dd></div>
          <div><dt>Treasury fees</dt><dd><Amount v={s.treasury} /></dd></div>
        </dl>
      )}

      {s && (
        <Card title={`Outcomes across ${s.deals.toLocaleString()} deals`} actions={<a href={href('/live')}>Watch live →</a>}>
          <Outcomes stats={s} />
        </Card>
      )}

      <div className="grid-2">
        <Card title="Deal mix">
          {s && (
            <ul className="bars">
              {TYPES.map((t) => {
                const n = s.deals_by_type?.[t] ?? 0;
                const p = policies().find((x) => x.deal_type === t)!;
                return (
                  <li className="bar-row" key={t}>
                    <TypeBadge type={t} />
                    <span className="bar" aria-hidden><span className="bar-fill" style={{ transform: `scaleX(${n / maxType})` }} /></span>
                    <span className="bar-n">{n.toLocaleString()}<span className="sr-only"> deals</span></span>
                    <span className="muted small bar-p">{p.fee_bps / 100}% fee{p.seller_bond_bps ? ` · ${p.seller_bond_bps / 100}% bond` : ''}</span>
                  </li>
                );
              })}
            </ul>
          )}
        </Card>

        <Card title={<><span className="live-dot" aria-hidden /> Live tape</>}>
          {feed.length === 0 ? <Empty>Waiting for network events…</Empty> : (
            <ul className="list tape">
              {feed.slice(0, 8).map((f) => (
                <li key={f.id} className={`feed-${f.kind}`}>
                  <a className="row-link" href={f.link ? href(f.link) : undefined}>
                    <span className="feed-dot" aria-hidden />
                    <span className="grow ellipsis">{f.text}</span>
                    <span className="muted small">{timeAgo(f.at)}</span>
                  </a>
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>

      <div className="grid-2">
        <Card title="Latest blocks" actions={<a href={href('/explorer')}>Ledger →</a>}>
          {!blocks.data ? <Loading /> : (
            <ol className="strata compact">
              {blocks.data.map((b) => (
                <li key={b.hash} style={{ ['--tx' as string]: Math.min(b.header.tx_count, 8) }}>
                  <a href={href(`/explorer/block/${b.header.height}`)}>
                    <span className="blk">#{b.header.height}</span>
                    <Hash h={b.hash} n={8} />
                    <span className="grow" />
                    <span className="muted small">{b.header.tx_count} tx · {timeAgo(b.header.timestamp)}</span>
                  </a>
                </li>
              ))}
            </ol>
          )}
        </Card>

        {deals.length > 0 && (
          <Card title="Your recent deals" actions={<a href={href('/deals')}>All →</a>}>
            <ul className="list">
              {deals.slice(0, 6).map((d) => {
                const other = d.seller === active?.address ? d.buyer : d.seller;
                return (
                  <li key={d.id}>
                    <a className="row-link" href={href(`/deals/${d.id}`)}>
                      <MoneyTrack d={d} />
                      <span className="grow ellipsis">{d.title}</span>
                      <StatusBadge status={d.status} />
                      <span className="small hide-sm"><Addr a={other} link={false} /></span>
                      <Amount v={d.amount} unit={false} />
                    </a>
                  </li>
                );
              })}
            </ul>
          </Card>
        )}
      </div>
    </div>
  );
}
