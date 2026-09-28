import { useMemo, useState } from 'react';
import AccountPicker from '../components/AccountPicker';
import { Amount, Card, Empty, KindTag, TypeBadge } from '../components/ui';
import { fmtAmount, fmtDuration, parseAmount } from '../lib/format';
import { href, navigate } from '../lib/router';
import { useStore } from '../lib/store';
import type { LineItem, Role } from '../lib/types';
import { classifyDeal, quoteDeal } from '../lib/wasm';

interface Row { name: string; qty: string; price: string }

export default function NewDeal() {
  const { active, me, directory, submit, busy } = useStore();
  const [role, setRole] = useState<Role>('seller');
  const [counterparty, setCounterparty] = useState('');
  const [arbiter, setArbiter] = useState('');
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [rows, setRows] = useState<Row[]>([{ name: '', qty: '1', price: '' }]);

  const cp = counterparty ? directory.get(counterparty) : undefined;

  const items: LineItem[] = rows
    .map((r) => ({ name: r.name.trim(), qty: parseInt(r.qty, 10), unit_price: parseAmount(r.price) }))
    .filter((i) => i.name && i.qty > 0 && i.unit_price > 0);

  const quote = useMemo(() => {
    if (!me || !cp) return null;
    const [s, b] = role === 'seller' ? [me.kind, cp.kind] : [cp.kind, me.kind];
    return quoteDeal(classifyDeal(s, b), items);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [me, cp, role, JSON.stringify(items)]);

  if (!active || !me) {
    return <div className="page"><Card><Empty>You need a registered wallet to propose deals. <a href={href('/wallet')}>Set one up →</a></Empty></Card></div>;
  }

  const p = quote?.policy;
  const needArbiter = !!p?.arbiter_required && !arbiter;
  const allRowsValid = rows.every((r) => r.name.trim() && parseInt(r.qty, 10) > 0 && parseAmount(r.price) > 0);
  const buyerFunds = role === 'buyer' ? me.balance : cp?.balance ?? 0;
  const bondShort = role === 'seller' && quote && quote.bond > me.balance;
  const canSubmit = cp && title.trim() && items.length > 0 && allRowsValid && !needArbiter && !bondShort && !busy;

  const set = (i: number, k: keyof Row, v: string) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, [k]: v } : r)));

  const create = async () => {
    try {
      const rec = await submit({
        type: 'create_deal',
        role,
        counterparty,
        title: title.trim(),
        description: description.trim(),
        items,
        arbiter: arbiter || null,
      });
      if (rec.deal_id) navigate(`/deals/${rec.deal_id}`);
    } catch {
      /* toast shown */
    }
  };

  return (
    <div className="page">
      <div className="page-h">
        <h1>Propose a deal</h1>
        <a href={href('/deals')}>← Back to deals</a>
      </div>
      <div className="grid-main">
        <div className="stack">
          <Card title="1 · Parties">
            <div className="form">
              <div className="seg">
                <button className={role === 'seller' ? 'on' : ''} onClick={() => setRole('seller')}>I'm selling</button>
                <button className={role === 'buyer' ? 'on' : ''} onClick={() => setRole('buyer')}>I'm buying</button>
              </div>
              <AccountPicker
                label={role === 'seller' ? 'Buyer' : 'Seller'}
                value={counterparty}
                exclude={[active.address]}
                onChange={(a) => { setCounterparty(a); if (a === arbiter) setArbiter(''); }}
              />
              <AccountPicker
                label={p?.arbiter_required ? `Arbiter (required for ${p.deal_type})` : 'Arbiter (optional)'}
                value={arbiter}
                exclude={[active.address, counterparty]}
                onChange={setArbiter}
                placeholder="Search arbiters, e.g. “Arbitration”"
                noneLabel={p?.arbiter_required ? undefined : 'No arbiter (disputes disabled)'}
              />
            </div>
          </Card>

          <Card title="2 · Terms">
            <div className="form">
              <label>Title<input value={title} onChange={(e) => setTitle(e.target.value)} maxLength={120} placeholder="What is being traded?" /></label>
              <label>Description<textarea value={description} onChange={(e) => setDescription(e.target.value)} rows={3} maxLength={2000} placeholder="Delivery terms, condition, warranty…" /></label>
              <div className="items">
                <div className="items-h"><span>Item</span><span>Qty</span><span>Unit price</span><span className="r">Line total</span><span /></div>
                {rows.map((r, i) => {
                  const line = (parseInt(r.qty, 10) || 0) * (parseAmount(r.price) || 0);
                  return (
                    <div className="items-r" key={i}>
                      <input value={r.name} onChange={(e) => set(i, 'name', e.target.value)} placeholder="Description" />
                      <input value={r.qty} onChange={(e) => set(i, 'qty', e.target.value.replace(/\D/g, ''))} inputMode="numeric" />
                      <input value={r.price} onChange={(e) => set(i, 'price', e.target.value)} placeholder="0.00" inputMode="decimal" />
                      <span className="r mono">{fmtAmount(line)}</span>
                      <button className="btn xs ghost" disabled={rows.length === 1} onClick={() => setRows((rs) => rs.filter((_, j) => j !== i))} aria-label="Remove line">✕</button>
                    </div>
                  );
                })}
                <button className="btn xs" onClick={() => setRows((rs) => [...rs, { name: '', qty: '1', price: '' }])}>+ Add line</button>
              </div>
            </div>
          </Card>
        </div>

        <div className="stack sticky">
          <Card title="Contract preview">
            {!quote ? <Empty>Choose a counterparty to see which rules apply.</Empty> : (
              <div className="quote">
                <div className="quote-type">
                  <TypeBadge type={quote.deal_type} />
                  <span>{me.name} <KindTag kind={me.kind} /> {role === 'seller' ? '→' : '←'} {cp?.name} {cp && <KindTag kind={cp.kind} />}</span>
                </div>
                <p className="muted small">{quote.policy.summary}</p>
                <dl className="kv">
                  <dt>Escrow amount</dt><dd><Amount v={quote.amount} /></dd>
                  <dt>Protocol fee ({quote.policy.fee_bps / 100}%)</dt><dd>−{fmtAmount(quote.fee)}</dd>
                  <dt>Seller receives</dt><dd><b><Amount v={quote.seller_payout} /></b></dd>
                  {quote.bond > 0 && <><dt>Seller bond ({quote.policy.seller_bond_bps / 100}%)</dt><dd>{fmtAmount(quote.bond)} locked</dd></>}
                  <dt>Accept / fund / deliver within</dt><dd>{fmtDuration(quote.policy.accept_window_secs)} · {fmtDuration(quote.policy.fund_window_secs)} · {fmtDuration(quote.policy.delivery_window_secs)}</dd>
                  <dt>Inspection window</dt><dd>{fmtDuration(quote.policy.release_window_secs)}</dd>
                  <dt>Buyer withdrawal</dt><dd>{quote.policy.buyer_can_withdraw ? 'Before delivery' : 'No'}</dd>
                </dl>
                <p className="tiny muted">Computed in your browser by the same Rust contract code the node executes.</p>
                {quote.amount > buyerFunds && <div className="note warn small">Buyer's current balance ({fmtAmount(buyerFunds)}) won't cover this escrow.</div>}
                {bondShort && <div className="note err small">You need {fmtAmount(quote.bond)} DEAL free to lock the performance bond.</div>}
              </div>
            )}
            <button className="btn primary block" disabled={!canSubmit} onClick={create}>
              {busy ? 'Waiting for block…' : 'Sign & propose deal'}
            </button>
          </Card>
        </div>
      </div>
    </div>
  );
}
