import { useEffect, useState } from 'react';
import { Card, Loading } from '../components/ui';
import { api } from '../lib/api';
import { STATUS_LABEL } from '../lib/format';
import { href, navigate, useQuery } from '../lib/router';
import type { DocumentVerification } from '../lib/types';
import { checkDocument } from '../lib/wasm';

interface Local { signature_ok: boolean; hash_ok: boolean; proofs: number; proofs_ok: number }

/**
 * Document verification — the page a contract's QR code opens. Two
 * independent answers:
 *   1. the market rebuilds the record from its ledger and checks it;
 *   2. the browser (Rust/WASM) re-checks the market signature against the
 *      market's published key and every block-inclusion proof.
 */
export default function Verify() {
  const q = useQuery();
  const deal = q.get('deal') ?? '';
  const hash = (q.get('hash') ?? '').toLowerCase();
  const sig = (q.get('sig') ?? '').toLowerCase();
  const [link, setLink] = useState('');
  const [server, setServer] = useState<DocumentVerification | null>(null);
  const [local, setLocal] = useState<Local | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setServer(null);
    setLocal(null);
    setErr(null);
    if (!deal || !hash || !sig) return;
    api.verifyDocument(deal, hash, sig).then(setServer).catch((e) => setErr(e.message));
    // Independent browser-side check: the presented hash + signature against
    // the market's published key, and the current record's inclusion proofs.
    Promise.all([api.attestation(), api.document(deal)])
      .then(([att, cur]) => {
        const c = checkDocument({ record: cur.record, hash, attestation: { public_key: att.public_key, signature: sig } });
        setLocal({ signature_ok: c.signature_ok, hash_ok: c.hash_ok, proofs: c.events.length, proofs_ok: c.events.filter((e) => e.proof_ok).length });
      })
      .catch(() => setLocal(null));
  }, [deal, hash, sig]);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const i = link.indexOf('#/verify?');
    const qs = i >= 0 ? link.slice(i + '#/verify?'.length) : link.trim();
    navigate(`/verify?${qs}`);
  };

  const superseded = server && !server.valid && server.final === false && server.checks.find((c) => c.name === 'Content matches the chain')?.ok === false;
  const verdict = !server ? null : server.valid ? 'valid' : superseded ? 'superseded' : 'invalid';

  return (
    <div className="page verify-page">
      <div>
        <span className="eyebrow">Document verification</span>
        <h1>Is this ChainDeal contract authentic?</h1>
        <p className="lead">Scan the QR code on a contract, or paste its verification link. Documents can only be verified here, against the ledger and key of the marketplace that issued them.</p>
      </div>

      <form className="verify-form" onSubmit={submit}>
        <label className="sr-only" htmlFor="vlink">Verification link</label>
        <input id="vlink" value={link} onChange={(e) => setLink(e.target.value)} placeholder="https://chaindeal.localhost/#/verify?deal=…&hash=…&sig=…" />
        <button className="btn primary" disabled={!link.trim()}>Verify</button>
      </form>

      {err && <div className="note err">{err}</div>}
      {deal && hash && sig && !server && !err && <Loading />}

      {server && (
        <section className={`verdict verdict-${verdict}`} role="status">
          <div className="verdict-seal" aria-hidden>{verdict === 'valid' ? '✓' : verdict === 'superseded' ? '↻' : '✗'}</div>
          <div className="grow">
            <p className="verdict-title">
              {verdict === 'valid' ? 'Authentic — issued by this marketplace' : verdict === 'superseded' ? 'Superseded — the deal has progressed' : 'Not valid'}
            </p>
            <p className="doc-mono">{server.number}</p>
            {server.status && <p className="muted small">Deal {deal} · current status: {STATUS_LABEL[server.status]}{server.final ? ' (final)' : ''}</p>}
          </div>
          {verdict !== 'invalid' && (
            <div className="row gap">
              <a className="btn sm" href={href(`/deals/${deal}/document?std=ru`)}>Open (RU)</a>
              <a className="btn sm" href={href(`/deals/${deal}/document?std=us`)}>Open (US)</a>
            </div>
          )}
        </section>
      )}

      {server && (
        <div className="grid-2">
          <Card title="Checked by the marketplace">
            <ul className="checks">
              {server.checks.map((c) => (
                <li key={c.name} className={c.ok ? 'ok' : 'bad'}><b>{c.ok ? '✓' : '✗'} {c.name}</b><span className="muted small">{c.detail}</span></li>
              ))}
            </ul>
          </Card>
          <Card title="Checked in your browser (Rust/WASM)">
            {!local ? <Loading /> : (
              <ul className="checks">
                <li className={local.signature_ok ? 'ok' : 'bad'}><b>{local.signature_ok ? '✓' : '✗'} Marketplace signature</b><span className="muted small">Verified locally against the published attestation key and this ledger's genesis hash.</span></li>
                <li className={local.hash_ok ? 'ok' : 'bad'}><b>{local.hash_ok ? '✓' : '✗'} Record hash</b><span className="muted small">The record served today hashes to the value printed on the document.</span></li>
                <li className={local.proofs_ok === local.proofs ? 'ok' : 'bad'}><b>{local.proofs_ok === local.proofs ? '✓' : '✗'} Block-inclusion proofs</b><span className="muted small">{local.proofs_ok} of {local.proofs} Merkle proofs recomputed in the browser.</span></li>
              </ul>
            )}
          </Card>
        </div>
      )}
    </div>
  );
}
