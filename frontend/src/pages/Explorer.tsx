import { useRef, useState } from 'react';
import { Addr, Card, Copy, Empty, ErrorNote, Hash, Loading, useLive } from '../components/ui';
import { api } from '../lib/api';
import { ACTION_LABEL, fmtAmount, fmtTime, timeAgo } from '../lib/format';
import { href } from '../lib/router';
import type { Block, TxRecord } from '../lib/types';

interface VerifyRun {
  running: boolean;
  blocks: number;
  txs: number;
  tip: number;
  errors: string[];
  ms: number;
}
import { verifyBlock, verifyTx } from '../lib/wasm';

export default function Explorer() {
  const [before, setBefore] = useState<number | undefined>();
  const blocks = useLive(() => api.blocks(before, 15), [before]);
  const txs = useLive(() => api.txs(15));
  const [report, setReport] = useState<VerifyRun | null>(null);
  const cancel = useRef(false);
  const [q, setQ] = useState('');

  const search = () => {
    const s = q.trim();
    if (/^\d+$/.test(s)) location.hash = `/explorer/block/${s}`;
    else if (/^D-/.test(s)) location.hash = `/deals/${s}`;
    else if (/^0x[0-9a-f]{40}$/i.test(s)) location.hash = `/accounts/${s}`;
    else if (/^[0-9a-f]{64}$/i.test(s)) location.hash = `/explorer/tx/${s}`;
  };

  // Walk the whole chain in chunks so a million signatures don't need one request.
  const verifyAll = async () => {
    cancel.current = false;
    const run: VerifyRun = { running: true, blocks: 0, txs: 0, tip: 0, errors: [], ms: 0 };
    setReport({ ...run });
    const t0 = performance.now();
    let from = 0;
    let prev: string | null = null;
    try {
      while (!cancel.current) {
        const r = await api.verifyRange(from, 500, prev);
        run.blocks += r.blocks_checked;
        run.txs += r.txs_checked;
        run.tip = r.tip_height;
        run.errors.push(...r.errors);
        run.ms = performance.now() - t0;
        setReport({ ...run });
        if (r.done || r.errors.length || r.blocks_checked === 0) break;
        from = r.next_from;
        prev = r.last_hash;
      }
    } catch (e) {
      run.errors.push(`verification aborted: ${(e as Error).message}`);
    }
    setReport({ ...run, running: false });
  };
  const checking = report?.running ?? false;

  const list = blocks.data ?? [];
  return (
    <div className="page">
      <div className="page-h">
        <h1>Ledger</h1>
        <div className="row gap">
          <input className="search" value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && search()} placeholder="Block #, tx hash, address or deal id" />
          {checking
            ? <button className="btn" onClick={() => (cancel.current = true)}>Stop verifying</button>
            : <button className="btn" onClick={verifyAll}>Verify entire chain</button>}
        </div>
      </div>

      {report && (
        <div className={`note ${report.errors.length ? 'err' : report.running ? 'info' : 'ok'}`} role="status">
          {report.running && (
            <div className="progress-bar" aria-hidden><span style={{ transform: `scaleX(${report.tip ? report.blocks / (report.tip + 1) : 0})` }} /></div>
          )}
          {report.errors.length
            ? <>✗ Chain verification failed:<ul>{report.errors.slice(0, 20).map((e, i) => <li key={i}>{e}</li>)}</ul></>
            : <>
                {report.running ? 'Verifying… ' : '✓ Chain intact — '}
                re-derived <b>{report.blocks.toLocaleString()}</b>{report.tip ? ` of ${(report.tip + 1).toLocaleString()}` : ''} block hashes, proofs-of-work, links and Merkle roots,
                and re-verified <b>{report.txs.toLocaleString()}</b> ed25519 signatures in {(report.ms / 1000).toFixed(1)} s
                {report.ms > 0 && <> ({Math.round(report.txs / (report.ms / 1000)).toLocaleString()} sig/s)</>}.
              </>}
        </div>
      )}

      <div className="grid-2">
        <Card title="Blocks" actions={
          <div className="row gap">
            {before != null && <button className="btn xs" onClick={() => setBefore(undefined)}>Latest</button>}
            {list.length > 0 && list[list.length - 1].header.height > 0 && (
              <button className="btn xs" onClick={() => setBefore(list[list.length - 1].header.height)}>Older →</button>
            )}
          </div>
        }>
          {blocks.loading ? <Loading /> : (
            // Each block is a slab; its thickness is its transaction count.
            <ol className="strata">
              {list.map((b) => (
                <li key={b.hash} style={{ ['--tx' as string]: Math.min(b.header.tx_count, 8) }}>
                  <a href={href(`/explorer/block/${b.header.height}`)}>
                    <span className="blk">#{b.header.height}</span>
                    <Hash h={b.hash} n={10} />
                    <span className="grow" />
                    <span className="small">{b.header.tx_count} tx</span>
                    <span className="mono small muted hide-sm nonce-col">nonce {b.header.nonce.toLocaleString()}</span>
                    <span className="muted small">{timeAgo(b.header.timestamp)}</span>
                  </a>
                </li>
              ))}
            </ol>
          )}
        </Card>

        <Card title="Latest transactions">
          {txs.loading ? <Loading /> : (txs.data ?? []).length === 0 ? <Empty>No transactions yet.</Empty> : <TxTable txs={txs.data!} />}
        </Card>
      </div>
    </div>
  );
}

function TxTable({ txs }: { txs: TxRecord[] }) {
  return (
    <table className="table">
      <thead><tr><th>Tx</th><th>Action</th><th>From</th><th className="r">Status</th></tr></thead>
      <tbody>
        {txs.map((t) => (
          <tr key={t.hash} className="clickable" onClick={() => (location.hash = `/explorer/tx/${t.hash}`)}>
            <td><Hash h={t.hash} n={6} to={`/explorer/tx/${t.hash}`} /></td>
            <td>{ACTION_LABEL[t.body.action.type]}</td>
            <td><Addr a={t.body.from} /></td>
            <td className="r"><TxStatusTag t={t} /></td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function TxStatusTag({ t }: { t: TxRecord }) {
  if (t.status === 'confirmed') return <span className="tag ok">#{t.block_height}</span>;
  if (t.status === 'rejected') return <span className="tag err" title={t.error ?? ''}>rejected</span>;
  return <span className="tag warn">pending</span>;
}

/** Runs the Rust verifier (WASM) locally over data the node served. */
function LocalCheck({ run, what }: { run: () => string[]; what: string }) {
  const [res, setRes] = useState<string[] | null>(null);
  return (
    <div className="local-check">
      <button className="btn xs" onClick={() => setRes(run())}>Verify {what} in my browser</button>
      {res && (res.length === 0
        ? <span className="tag ok">✓ valid — checked locally with Rust/WASM</span>
        : <span className="tag err">✗ {res.join('; ')}</span>)}
    </div>
  );
}

export function BlockView({ id }: { id: string }) {
  const r = useLive(() => api.block(id), [id]);
  if (r.error) return <div className="page"><ErrorNote msg={r.error} /></div>;
  if (!r.data) return <div className="page"><Loading /></div>;
  const { block: b, txs } = r.data;
  const h = b.header;
  const check = (bl: Block) => {
    const errs: string[] = [];
    const e = verifyBlock(bl);
    if (e) errs.push(e);
    for (const t of txs) {
      const te = verifyTx(t);
      if (te) errs.push(`tx ${t.hash.slice(0, 8)}: ${te}`);
    }
    return errs;
  };
  return (
    <div className="page">
      <div className="crumbs"><a href={href('/explorer')}>Explorer</a> / block</div>
      <div className="page-h">
        <h1>Block #{h.height}</h1>
        <div className="row gap">
          {h.height > 0 && <a className="btn xs" href={href(`/explorer/block/${h.height - 1}`)}>← #{h.height - 1}</a>}
          <a className="btn xs" href={href(`/explorer/block/${h.height + 1}`)}>#{h.height + 1} →</a>
        </div>
      </div>
      <Card title="Header" actions={<LocalCheck what="block & signatures" run={() => check(b)} />}>
        <dl className="kv wide">
          <dt>Hash</dt><dd><code className="hash">{b.hash}</code> <Copy text={b.hash} /></dd>
          <dt>Previous</dt><dd>{h.height > 0 ? <a href={href(`/explorer/block/${h.height - 1}`)}><code className="hash">{h.prev_hash}</code></a> : <code className="hash">{h.prev_hash}</code>}</dd>
          <dt>Merkle root</dt><dd><code className="hash">{h.merkle_root}</code></dd>
          <dt>Timestamp</dt><dd>{fmtTime(h.timestamp)} ({timeAgo(h.timestamp)})</dd>
          <dt>Proof of work</dt><dd>nonce <b>{h.nonce.toLocaleString()}</b> · difficulty {h.difficulty} (hash starts with {'0'.repeat(h.difficulty)})</dd>
          <dt>Producer</dt><dd>{h.producer}</dd>
          <dt>Transactions</dt><dd>{h.tx_count}</dd>
        </dl>
      </Card>
      <Card title="Transactions">
        {txs.length === 0 ? <Empty>{h.height === 0 ? 'Genesis block — no transactions.' : 'Empty block.'}</Empty> : <TxTable txs={txs} />}
      </Card>
    </div>
  );
}

export function TxView({ hash }: { hash: string }) {
  const r = useLive(() => api.tx(hash), [hash]);
  if (r.error) return <div className="page"><ErrorNote msg={r.error} /></div>;
  if (!r.data) return <div className="page"><Loading /></div>;
  const t = r.data;
  const a = t.body.action;
  return (
    <div className="page">
      <div className="crumbs"><a href={href('/explorer')}>Explorer</a> / transaction</div>
      <div className="page-h">
        <h1>{ACTION_LABEL[a.type]}</h1>
        <TxStatusTag t={t} />
      </div>
      {t.status === 'rejected' && <div className="note err">Rejected by the contract: {t.error}</div>}
      <Card title="Summary" actions={<LocalCheck what="signature" run={() => { const e = verifyTx(t); return e ? [e] : []; }} />}>
        <dl className="kv wide">
          <dt>Hash</dt><dd><code className="hash">{t.hash}</code> <Copy text={t.hash} /></dd>
          <dt>Status</dt><dd>{t.status}{t.block_height != null && <> in <a href={href(`/explorer/block/${t.block_height}`)}>block #{t.block_height}</a></>}</dd>
          <dt>From</dt><dd><Addr a={t.body.from} /></dd>
          {t.deal_id && <><dt>Deal</dt><dd><a href={href(`/deals/${t.deal_id}`)}>{t.deal_id}</a></dd></>}
          {'to' in a && <><dt>To</dt><dd><Addr a={a.to} /></dd></>}
          {'amount' in a && <><dt>Amount</dt><dd>{fmtAmount(a.amount)} DEAL</dd></>}
          <dt>Signed at</dt><dd>{fmtTime(t.body.timestamp / 1000)}</dd>
          <dt>Public key</dt><dd><code className="hash">{t.body.pubkey}</code></dd>
          <dt>Signature</dt><dd><code className="hash wrap">{t.signature}</code></dd>
        </dl>
      </Card>
      <Card title="Signed payload">
        <pre className="json">{JSON.stringify(t.body, null, 2)}</pre>
      </Card>
    </div>
  );
}
