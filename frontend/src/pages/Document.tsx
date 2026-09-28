import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { ErrorNote, Loading } from '../components/ui';
import { api } from '../lib/api';
import {
  enAmount, enDate, enDateTime, enMoney, ruAmount, ruDate, ruDateLong, ruDateTime, ruMoney,
} from '../lib/docfmt';
import { href, navigate, useQuery } from '../lib/router';
import type { ContractDocument, DealRecord, DealStatus, EventRecord, PartyRecord } from '../lib/types';
import { checkDocument, policies, qrSvg } from '../lib/wasm';

/*
 * Contract documents: a paper rendering of an attested deal record.
 *   RU — «Договор купли-продажи» laid out after ГОСТ Р 7.0.97-2016 (A4, fields
 *        20/10/20/20 mm, numbered sections, реквизиты и подписи, акт об исполнении).
 *   US — "Purchase and Sale Agreement" in US drafting style (Letter, 1" margins,
 *        recitals, numbered sections, signature blocks, settlement statement).
 * Both render the same record, so both carry the same document number; the
 * certificate block lets anyone verify it — but only against this market.
 */

type Std = 'ru' | 'us';

const OUTCOME_RU: Record<DealStatus, string> = {
  proposed: 'Проект договора (оферта не акцептована)', accepted: 'Договор заключён, ожидается оплата',
  funded: 'Оплата депонирована, ожидается передача', shipped: 'Товар передан, идёт приёмка',
  disputed: 'Спор передан арбитру', completed: 'Договор исполнен сторонами в полном объёме',
  resolved: 'Спор разрешён арбитром, расчёты произведены', cancelled: 'Договор расторгнут',
  declined: 'Договор не заключён: отказ контрагента', expired: 'Договор прекращён: истёк срок',
  failed: 'Договор расторгнут вследствие неисполнения обязательств Продавцом',
};
const OUTCOME_EN: Record<DealStatus, string> = {
  proposed: 'Draft — offer not yet accepted', accepted: 'Executed — awaiting payment into escrow',
  funded: 'Payment held in escrow — awaiting delivery', shipped: 'Delivered — inspection period running',
  disputed: 'Dispute referred to the arbiter', completed: 'Performed in full by both Parties',
  resolved: 'Dispute resolved by arbitration; escrow disbursed', cancelled: 'Terminated (cancelled)',
  declined: 'Not concluded — offer declined', expired: 'Lapsed — deadline passed',
  failed: 'Terminated — Seller default (failure to deliver)',
};
const EVENT_RU: Record<DealStatus, string> = {
  proposed: 'Оферта направлена', accepted: 'Оферта акцептована', funded: 'Оплата депонирована', shipped: 'Товар передан',
  completed: 'Приёмка подтверждена, расчёт произведён', disputed: 'Заявлен спор', resolved: 'Решение арбитра исполнено',
  cancelled: 'Договор расторгнут', declined: 'Отказ от заключения', expired: 'Истечение срока', failed: 'Неисполнение, возврат оплаты',
};
const EVENT_EN: Record<DealStatus, string> = {
  proposed: 'Offer made', accepted: 'Offer accepted', funded: 'Price deposited in escrow', shipped: 'Delivery made',
  completed: 'Acceptance confirmed; escrow released', disputed: 'Dispute raised', resolved: 'Arbitral decision executed',
  cancelled: 'Agreement cancelled', declined: 'Offer declined', expired: 'Deadline lapsed', failed: 'Seller default; refund made',
};

const short = (h: string, n = 10) => `${h.slice(0, n)}…${h.slice(-6)}`;
/** Percent from basis points, Russian style: 50 → «0,5 %», 4000 → «40 %». */
const ruPct = (bps: number) => `${(bps / 100).toLocaleString('ru-RU', { maximumFractionDigits: 2 })} %`;
const signingEvent = (r: DealRecord, who: string) => r.events.find((e) => e.by === who);
const partyRole = (r: DealRecord, a: string, std: Std) =>
  a === r.seller.address ? (std === 'ru' ? 'Продавец' : 'Seller') : a === r.buyer.address ? (std === 'ru' ? 'Покупатель' : 'Buyer') : (std === 'ru' ? 'Арбитр' : 'Arbiter');

// ---- Russian: ДОГОВОР КУПЛИ-ПРОДАЖИ -----------------------------------------------

function ruKind(p: PartyRecord) {
  return p.kind === 'business' ? 'юридическое лицо' : 'физическое лицо';
}
function ruNamed(p: PartyRecord, role: string) {
  const verb = p.kind === 'business' ? 'именуемое' : 'именуемый(-ая)';
  // Addresses go to «Реквизиты сторон» (as is customary), keeping the justified preamble clean.
  return <><b>{p.name}</b> ({ruKind(p)}), {verb} в дальнейшем «{role}»</>;
}

function RuContract({ d }: { d: ContractDocument }) {
  const r = d.record;
  const p = policies().find((x) => x.deal_type === r.deal_type)!;
  const total = r.settlement.amount;
  const s = r.settlement;
  return (
    <>
      <p className="doc-center doc-title">ДОГОВОР КУПЛИ-ПРОДАЖИ<br />№ {d.number}</p>
      <div className="doc-place">
        <span>Электронная торговая площадка ChainDeal</span>
        <span>{ruDateLong(r.created_at)}</span>
      </div>

      <p className="doc-p">
        {ruNamed(r.seller, 'Продавец')}, с одной стороны, и {ruNamed(r.buyer, 'Покупатель')}, с другой стороны, совместно
        именуемые «Стороны», заключили настоящий Договор (далее — «Договор») о нижеследующем:
      </p>

      <h3 className="doc-h">1. ПРЕДМЕТ ДОГОВОРА</h3>
      <p className="doc-p">1.1. Продавец обязуется передать в собственность Покупателя, а Покупатель обязуется принять и оплатить товары (работы, услуги) согласно Спецификации (п. 1.2) — «{r.title}».</p>
      {r.description && <p className="doc-p">1.2. Условия: {r.description}</p>}
      <p className="doc-p">{r.description ? '1.3' : '1.2'}. Спецификация:</p>
      <table className="doc-table">
        <thead><tr><th>№ п/п</th><th>Наименование</th><th>Кол-во</th><th>Цена, DEAL</th><th>Сумма, DEAL</th></tr></thead>
        <tbody>
          {r.items.map((it, i) => (
            <tr key={i}><td className="c">{i + 1}</td><td>{it.name}</td><td className="c">{it.qty}</td><td className="r">{ruMoney(it.unit_price)}</td><td className="r">{ruMoney(it.qty * it.unit_price)}</td></tr>
          ))}
          <tr className="doc-total"><td colSpan={4}>Итого</td><td className="r">{ruMoney(total)}</td></tr>
        </tbody>
      </table>

      <h3 className="doc-h">2. ЦЕНА И ПОРЯДОК РАСЧЁТОВ</h3>
      <p className="doc-p">2.1. Цена Договора составляет {ruAmount(total)}.</p>
      <p className="doc-p">2.2. Расчёты производятся через механизм условного депонирования (эскроу) Площадки: Покупатель вносит цену Договора в полном объёме, и она удерживается Площадкой до исполнения обязательств Продавцом и подтверждения приёмки либо до решения арбитра.</p>
      <p className="doc-p">2.3. Вознаграждение Площадки составляет {ruPct(p.fee_bps)} и удерживается из суммы, причитающейся Продавцу.</p>

      <h3 className="doc-h">3. СРОКИ ИСПОЛНЕНИЯ И ПРИЁМКА</h3>
      <p className="doc-p">3.1. Акцепт оферты — в течение {p.accept_window_secs / 60} мин.; депонирование оплаты — в течение {p.fund_window_secs / 60} мин. после акцепта; передача товара — в течение {p.delivery_window_secs / 60} мин. после депонирования.</p>
      <p className="doc-p">3.2. Срок приёмки составляет {p.release_window_secs / 60} мин. с момента передачи. При отсутствии мотивированных претензий по его истечении депонированная сумма перечисляется Продавцу.</p>

      {s.bond > 0 && (
        <>
          <h3 className="doc-h">4. ОБЕСПЕЧЕНИЕ ИСПОЛНЕНИЯ</h3>
          <p className="doc-p">4.1. Продавец вносит обеспечительный платёж в размере {ruPct(p.seller_bond_bps)} цены Договора — {ruAmount(s.bond)}. Платёж возвращается Продавцу при надлежащем исполнении и переходит Покупателю при неисполнении обязательств Продавцом либо по решению арбитра в пользу Покупателя (более 50 % суммы).</p>
        </>
      )}

      <h3 className="doc-h">{s.bond > 0 ? 5 : 4}. ОТВЕТСТВЕННОСТЬ СТОРОН</h3>
      <p className="doc-p">Пропуск срока депонирования влечёт прекращение Договора с отметкой о неисполнении Покупателем. Пропуск срока передачи влечёт расторжение Договора, возврат оплаты Покупателю{s.bond > 0 ? ' и переход к нему обеспечительного платежа' : ''}.</p>

      <h3 className="doc-h">{s.bond > 0 ? 6 : 5}. РАЗРЕШЕНИЕ СПОРОВ</h3>
      <p className="doc-p">
        {r.arbiter
          ? <>Споры разрешаются арбитром Площадки — {r.arbiter.name}. Решение арбитра о распределении депонированной суммы исполняется Площадкой.</>
          : <>Арбитр Сторонами не назначен; при отсутствии претензий в срок приёмки расчёт производится автоматически.</>}
      </p>
      {p.buyer_can_withdraw && (
        <p className="doc-p">Покупатель-потребитель вправе отказаться от Договора до передачи товара с полным возвратом оплаты.</p>
      )}

      <h3 className="doc-h">{s.bond > 0 ? 7 : 6}. ЗАКЛЮЧИТЕЛЬНЫЕ ПОЛОЖЕНИЯ</h3>
      <p className="doc-p">Договор заключён в электронной форме. Каждое действие Сторон подписано электронной подписью (Ed25519) и зафиксировано в реестре Площадки; подлинность Договора подтверждается отметкой Площадки (см. «Отметка о регистрации»).</p>

      <h3 className="doc-h">{s.bond > 0 ? 8 : 7}. РЕКВИЗИТЫ И ПОДПИСИ СТОРОН</h3>
      <table className="doc-sign">
        <tbody>
          <tr>
            {[r.seller, r.buyer].map((pty) => {
              const ev = signingEvent(r, pty.address);
              return (
                <td key={pty.address}>
                  <b>{pty.address === r.seller.address ? 'ПРОДАВЕЦ' : 'ПОКУПАТЕЛЬ'}</b>
                  <p>{pty.name}<br /><span className="doc-small">{ruKind(pty)}</span></p>
                  <p className="doc-small">Адрес в сети:<br /><span className="doc-mono">{pty.address}</span></p>
                  <p className="doc-signline">{ev ? <>ЭП: <span className="doc-mono">{short(ev.tx_hash, 8)}</span></> : '________________'} / {pty.name} /</p>
                  {pty.kind === 'business' && <p className="doc-small">М.П.</p>}
                </td>
              );
            })}
          </tr>
        </tbody>
      </table>

      <div className="doc-pagebreak" />
      <p className="doc-center doc-title">АКТ<br /><span className="doc-subtitle">об исполнении Договора № {d.number}</span></p>
      <div className="doc-place"><span>Электронная торговая площадка ChainDeal</span><span>{ruDate(r.updated_at)}</span></div>
      <p className="doc-p">Результат: <b>{OUTCOME_RU[r.status]}</b>.</p>
      <table className="doc-table">
        <thead><tr><th>Дата и время</th><th>Действие</th><th>Сторона</th><th>Блок</th><th>Транзакция</th></tr></thead>
        <tbody>
          {r.events.map((e: EventRecord) => (
            <tr key={e.tx_hash}><td>{ruDateTime(e.at)}</td><td>{EVENT_RU[e.status]}</td><td>{partyRole(r, e.by, 'ru')}</td><td className="c">{e.block_height}</td><td className="doc-mono">{short(e.tx_hash)}</td></tr>
          ))}
        </tbody>
      </table>
      <table className="doc-table doc-kv">
        <tbody>
          <tr><td>Цена Договора</td><td className="r">{ruMoney(s.amount)} DEAL</td></tr>
          <tr><td>Перечислено Продавцу</td><td className="r">{ruMoney(s.to_seller)} DEAL</td></tr>
          <tr><td>Возвращено Покупателю</td><td className="r">{ruMoney(s.to_buyer)} DEAL</td></tr>
          <tr><td>Вознаграждение Площадки</td><td className="r">{ruMoney(s.fee)} DEAL</td></tr>
          {s.bond > 0 && <tr><td>Обеспечительный платёж ({ruMoney(s.bond)} DEAL)</td><td className="r">{s.bond_to === 'buyer' ? 'передан Покупателю' : s.bond_to === 'seller' ? 'возвращён Продавцу' : 'удерживается'}</td></tr>}
          {r.buyer_refund_bps != null && <tr><td>Решение арбитра</td><td className="r">{ruPct(r.buyer_refund_bps)} — Покупателю</td></tr>}
        </tbody>
      </table>
    </>
  );
}

// ---- US: PURCHASE AND SALE AGREEMENT -----------------------------------------------

const enKind = (p: PartyRecord) => (p.kind === 'business' ? 'a business entity' : 'an individual');

function UsContract({ d }: { d: ContractDocument }) {
  const r = d.record;
  const p = policies().find((x) => x.deal_type === r.deal_type)!;
  const s = r.settlement;
  let n = 0;
  const sec = (title: string, body: ReactNode) => (
    <p className="doc-p"><b>{++n}. {title}.</b> {body}</p>
  );
  return (
    <>
      <p className="doc-center doc-title">PURCHASE AND SALE AGREEMENT</p>
      <p className="doc-center doc-subtitle">Agreement No. {d.number}</p>

      <p className="doc-p">
        This Purchase and Sale Agreement (this “<b>Agreement</b>”) is made and entered into as of {enDate(r.created_at)} (the “<b>Effective Date</b>”),
        by and between <b>{r.seller.name.toUpperCase()}</b>, {enKind(r.seller)} (“<b>Seller</b>”), and <b>{r.buyer.name.toUpperCase()}</b>, {enKind(r.buyer)} (“<b>Buyer</b>”).
        Seller and Buyer are each referred to herein as a “<b>Party</b>” and together as the “<b>Parties</b>.”
      </p>

      <p className="doc-center doc-h">RECITALS</p>
      <p className="doc-p">WHEREAS, Seller desires to sell, and Buyer desires to purchase, the goods and/or services described in Schedule A (“{r.title}”);</p>
      <p className="doc-p">WHEREAS, the Parties have agreed to settle this transaction through the escrow facility of the ChainDeal marketplace (the “<b>Marketplace</b>”); and</p>
      <p className="doc-p">NOW, THEREFORE, in consideration of the mutual covenants contained herein, and for other good and valuable consideration, the receipt and sufficiency of which are hereby acknowledged, the Parties agree as follows:</p>

      {sec('Sale', <>Seller shall sell, transfer and deliver to Buyer, and Buyer shall purchase and accept from Seller, the items set forth in Schedule A.{r.description ? ` Additional terms: ${r.description}` : ''}</>)}
      <p className="doc-center doc-h">SCHEDULE A</p>
      <table className="doc-table">
        <thead><tr><th>No.</th><th>Description</th><th>Qty</th><th>Unit Price (DEAL)</th><th>Amount (DEAL)</th></tr></thead>
        <tbody>
          {r.items.map((it, i) => (
            <tr key={i}><td className="c">{i + 1}</td><td>{it.name}</td><td className="c">{it.qty}</td><td className="r">{enMoney(it.unit_price)}</td><td className="r">{enMoney(it.qty * it.unit_price)}</td></tr>
          ))}
          <tr className="doc-total"><td colSpan={4}>Total Purchase Price</td><td className="r">{enMoney(s.amount)}</td></tr>
        </tbody>
      </table>
      {sec('Purchase Price', <>The total purchase price is {enAmount(s.amount)} (the “<b>Purchase Price</b>”).</>)}
      {sec('Escrow', <>Buyer shall deposit the Purchase Price into escrow with the Marketplace within {p.fund_window_secs / 60} minutes after acceptance. Escrowed funds are released to Seller upon Buyer’s confirmation of receipt, or automatically upon expiry of the Inspection Period without dispute. A Marketplace fee of {p.fee_bps / 100}% is deducted from the amount payable to Seller.</>)}
      {sec('Delivery and Inspection', <>Seller shall deliver within {p.delivery_window_secs / 60} minutes after the deposit. Buyer shall have {p.release_window_secs / 60} minutes after delivery (the “<b>Inspection Period</b>”) to confirm receipt or raise a dispute.</>)}
      {s.bond > 0 && sec('Performance Bond', <>Seller has posted a performance bond of {p.seller_bond_bps / 100}% of the Purchase Price ({enMoney(s.bond)} DEAL), returned upon performance and forfeited to Buyer upon Seller’s default or an arbitral award of more than fifty percent (50%) in Buyer’s favor.</>)}
      {sec('Default; Termination', <>If Buyer fails to fund the escrow when due, this Agreement lapses. If Seller fails to deliver when due, this Agreement terminates, the Purchase Price is refunded to Buyer{s.bond > 0 ? ', and the performance bond is forfeited to Buyer' : ''}.</>)}
      {sec('Dispute Resolution', r.arbiter
        ? <>Any dispute shall be resolved by {r.arbiter.name} as arbiter. The arbiter’s allocation of the escrowed funds is final and binding and shall be executed by the Marketplace.</>
        : <>The Parties have not appointed an arbiter; absent a timely dispute, escrow is released upon expiry of the Inspection Period.</>)}
      {p.buyer_can_withdraw && sec('Consumer Right of Withdrawal', <>Buyer, as a consumer, may withdraw from this Agreement at any time before delivery and receive a full refund of the Purchase Price.</>)}
      {sec('Electronic Signatures and Records', <>The Parties consent to transact electronically. Each action under this Agreement is authenticated by the acting Party’s Ed25519 digital signature and recorded on the Marketplace ledger; such signatures and records have the same effect as handwritten signatures and paper records, consistent with the federal ESIGN Act (15 U.S.C. § 7001 et seq.) and the Uniform Electronic Transactions Act.</>)}
      {sec('Entire Agreement', <>This Agreement, including Schedule A and the Settlement Statement, constitutes the entire agreement of the Parties. In the event of any conflict, the ledger record identified in the Certificate of Record controls.</>)}

      <p className="doc-p">IN WITNESS WHEREOF, the Parties have executed this Agreement as of the Effective Date.</p>
      <table className="doc-sign">
        <tbody>
          <tr>
            {[r.seller, r.buyer].map((pty) => {
              const ev = signingEvent(r, pty.address);
              return (
                <td key={pty.address}>
                  <b>{pty.address === r.seller.address ? 'SELLER' : 'BUYER'}:</b>
                  <p className="doc-signline">By: {ev ? <span className="doc-mono">/s/ {short(ev.tx_hash, 12)}</span> : '____________________'}</p>
                  <p>Name: {pty.name}<br />{pty.kind === 'business' ? 'Title: Authorized Signatory' : 'Individually'}</p>
                  <p className="doc-small">Date: {ev ? enDate(ev.at) : '__________'}<br />Wallet: <span className="doc-mono">{pty.address}</span></p>
                </td>
              );
            })}
          </tr>
        </tbody>
      </table>

      <div className="doc-pagebreak" />
      <p className="doc-center doc-title">SETTLEMENT STATEMENT</p>
      <p className="doc-center doc-subtitle">Agreement No. {d.number} · as of {enDate(r.updated_at)}</p>
      <p className="doc-p">Outcome: <b>{OUTCOME_EN[r.status]}</b>.</p>
      <table className="doc-table">
        <thead><tr><th>Date / Time</th><th>Event</th><th>Party</th><th>Block</th><th>Transaction</th></tr></thead>
        <tbody>
          {r.events.map((e) => (
            <tr key={e.tx_hash}><td>{enDateTime(e.at)}</td><td>{EVENT_EN[e.status]}</td><td>{partyRole(r, e.by, 'us')}</td><td className="c">{e.block_height}</td><td className="doc-mono">{short(e.tx_hash)}</td></tr>
          ))}
        </tbody>
      </table>
      <table className="doc-table doc-kv">
        <tbody>
          <tr><td>Purchase Price</td><td className="r">{enMoney(s.amount)} DEAL</td></tr>
          <tr><td>Disbursed to Seller</td><td className="r">{enMoney(s.to_seller)} DEAL</td></tr>
          <tr><td>Refunded to Buyer</td><td className="r">{enMoney(s.to_buyer)} DEAL</td></tr>
          <tr><td>Marketplace fee</td><td className="r">{enMoney(s.fee)} DEAL</td></tr>
          {s.bond > 0 && <tr><td>Performance bond ({enMoney(s.bond)} DEAL)</td><td className="r">{s.bond_to === 'buyer' ? 'Forfeited to Buyer' : s.bond_to === 'seller' ? 'Returned to Seller' : 'Held'}</td></tr>}
          {r.buyer_refund_bps != null && <tr><td>Arbitral award to Buyer</td><td className="r">{(r.buyer_refund_bps / 100).toFixed(2)}%</td></tr>}
        </tbody>
      </table>
    </>
  );
}

// ---- certificate (both standards) ---------------------------------------------------

/** The issuing market's public address (its OIDC issuer), not whatever host rendered the page. */
function useMarketOrigin() {
  const [origin, setOrigin] = useState(location.origin);
  useEffect(() => {
    fetch('/.well-known/openid-configuration')
      .then((r) => r.json())
      .then((c) => c.issuer && setOrigin(c.issuer))
      .catch(() => {});
  }, []);
  return origin;
}

function Certificate({ d, std }: { d: ContractDocument; std: Std }) {
  const url = useMarketOrigin() + d.verify_path;
  const qr = useMemo(() => qrSvg(url), [url]);
  const ru = std === 'ru';
  return (
    <section className="doc-cert">
      {/* Rendered as an image (not injected markup): the SVG comes from the Rust QR encoder. */}
      <img className="doc-qr" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(qr)}`} alt={ru ? 'QR-код для проверки подлинности' : 'QR code to verify this document'} />
      <div className="doc-cert-body">
        <p className="doc-cert-title">{ru ? 'ОТМЕТКА О РЕГИСТРАЦИИ В РЕЕСТРЕ ПЛОЩАДКИ' : 'CERTIFICATE OF RECORD'}</p>
        <table className="doc-cert-kv">
          <tbody>
            <tr><td>{ru ? 'Реестровый номер' : 'Record number'}</td><td className="doc-mono">{d.number}</td></tr>
            <tr><td>{ru ? 'Хеш записи (SHA-256)' : 'Record hash (SHA-256)'}</td><td className="doc-mono doc-wrap">{d.hash}</td></tr>
            <tr><td>{ru ? 'Идентификатор реестра' : 'Ledger (genesis)'}</td><td className="doc-mono">{short(d.record.chain_id, 16)}</td></tr>
            <tr><td>{ru ? 'Ключ площадки' : 'Marketplace key'}</td><td className="doc-mono">Ed25519 · {d.attestation.key_id}</td></tr>
            <tr><td>{ru ? 'Подпись площадки' : 'Marketplace signature'}</td><td className="doc-mono doc-wrap">{d.attestation.signature}</td></tr>
          </tbody>
        </table>
        <p className="doc-small">
          {ru
            ? 'Подлинность проверяется только на площадке ChainDeal, выпустившей документ: отсканируйте QR-код или откройте ссылку. Площадка восстанавливает запись из своего реестра, сверяет хеш, свою подпись и доказательства включения каждой транзакции в блок.'
            : 'Authenticity can be verified only on the ChainDeal marketplace that issued it: scan the QR code or open the link. The marketplace rebuilds the record from its ledger and checks the hash, its signature, and the block-inclusion proof of every transaction.'}
        </p>
        <p className="doc-small doc-mono doc-wrap">{url}</p>
      </div>
    </section>
  );
}

export default function DocumentPage({ id }: { id: string }) {
  const query = useQuery();
  const std: Std = query.get('std') === 'us' ? 'us' : query.get('std') === 'ru' ? 'ru' : navigator.language.startsWith('ru') ? 'ru' : 'us';
  const [doc, setDoc] = useState<ContractDocument | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    api.document(id).then(setDoc).catch((e) => setErr(e.message));
  }, [id]);
  const local = useMemo(() => (doc ? checkDocument(doc) : null), [doc]);

  if (err) return <div className="page"><ErrorNote msg={err} /></div>;
  if (!doc) return <div className="page"><Loading /></div>;
  const r = doc.record;
  const ru = std === 'ru';
  const proofs = local?.events.every((e) => e.proof_ok);
  return (
    <div className="page doc-page">
      <div className="doc-toolbar no-print">
        <a href={href(`/deals/${r.deal_id}`)}>← {r.title}</a>
        <span className="grow" />
        <div className="seg small" role="group" aria-label="Document standard">
          <button className={std === 'ru' ? 'on' : ''} aria-pressed={std === 'ru'} onClick={() => navigate(`/deals/${id}/document?std=ru`)}>RU · ГОСТ</button>
          <button className={std === 'us' ? 'on' : ''} aria-pressed={std === 'us'} onClick={() => navigate(`/deals/${id}/document?std=us`)}>US · Letter</button>
        </div>
        <button className="btn primary sm" onClick={() => window.print()}>Print / Save PDF</button>
        <a className="btn sm" href={doc.verify_path}>Verify</a>
      </div>
      <p className="doc-localcheck no-print">
        {local && local.hash_ok && local.signature_ok && proofs
          ? <>✓ Checked in your browser (Rust/WASM): record hash, marketplace signature and {local.events.length} block-inclusion proofs.</>
          : <>✗ In-browser check failed — do not rely on this document.</>}
        {!r.is_final && <> The deal is still open, so this is a <b>draft</b>; its number will change as the deal progresses.</>}
      </p>

      <article className={`doc-sheet doc-${std} ${r.is_final ? '' : 'doc-draft'}`} lang={ru ? 'ru' : 'en-US'} data-watermark={ru ? 'ПРОЕКТ' : 'DRAFT'}>
        {ru ? <RuContract d={doc} /> : <UsContract d={doc} />}
        <Certificate d={doc} std={std} />
        <p className="doc-disclaimer">
          {ru
            ? 'Демонстрационный документ, сформированный по записям демонстрационной сети ChainDeal. Не является юридически значимым документом.'
            : 'Demonstration document generated from records of the ChainDeal demo network. Not a legal instrument.'}
        </p>
      </article>
    </div>
  );
}
