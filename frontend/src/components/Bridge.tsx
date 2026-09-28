import { useEffect, useRef, type CSSProperties } from 'react';
import { fundsPosition, fundsSentence, type Spot } from '../lib/deal';
import { fmtAmount, fmtDuration } from '../lib/format';
import { useStore } from '../lib/store';
import type { Deal, DealPolicy } from '../lib/types';
import { Avatar, useNow } from './ui';

/**
 * The Custody Bridge — the product's signature view.
 *
 * A deal is drawn as a counter between buyer (left) and seller (right) with a
 * custody vault in the middle. Value is a physical ingot whose *position is the
 * contract state*: it waits with the buyer, slides into custody when funded and
 * crosses to the seller (or splits, on a ruling) when released. Because the
 * layout is a pure function of the deal, live block updates animate the ingot
 * via CSS transitions — motion only happens when money actually moves.
 */

const X: Record<Spot, number> = { buyer: 16, custody: 50, seller: 84 };

interface Props {
  d: Deal;
  policy: DealPolicy;
  /** Arbiter's live split preview in basis points to the buyer (disputed deals only). */
  previewBps?: number | null;
  /** A transaction for this deal is signed and waiting for its block. */
  pending?: boolean;
}

// Vertical geometry (px) for the physical stack; horizontal is % via --x.
const PRINCIPAL_H = 36;
const BOND_H = 22;
const CUSTODY_LIFT = 20; // items in custody sit on the vault shelf
const SHELF = 78; // bottom of the shelf, from the stage floor

const lane = (x: number, y = 0): CSSProperties => ({ ['--x' as string]: x, ['--y' as string]: y });

export default function Bridge({ d, policy, previewBps = null, pending = false }: Props) {
  const { nameOf, active } = useStore();
  const now = useNow();
  const stage = useRef<HTMLDivElement>(null);
  const f = fundsPosition(d);

  // Stamp the seal only when the status changes while the page is open —
  // never on first paint, so motion always means "something just happened".
  const prevStatus = useRef(d.status);
  const stampKey = useRef(0);
  if (prevStatus.current !== d.status) {
    stampKey.current += 1;
    prevStatus.current = d.status;
  }

  // Pointer parallax: rAF-throttled, writes CSS vars directly (no re-render).
  useEffect(() => {
    const el = stage.current;
    if (!el) return;
    const fine = matchMedia('(pointer: fine)').matches;
    const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (!fine || still) return;
    let frame = 0;
    const move = (e: PointerEvent) => {
      const r = el.getBoundingClientRect();
      const px = ((e.clientX - r.left) / r.width) * 2 - 1;
      const py = ((e.clientY - r.top) / r.height) * 2 - 1;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        el.style.setProperty('--px', px.toFixed(3));
        el.style.setProperty('--py', py.toFixed(3));
      });
    };
    const leave = () => {
      cancelAnimationFrame(frame);
      el.style.setProperty('--px', '0');
      el.style.setProperty('--py', '0');
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerleave', leave);
    return () => {
      cancelAnimationFrame(frame);
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerleave', leave);
    };
  }, []);

  // Split pieces: an actual ruling, or the arbiter's live preview.
  const splitBps = f.principal === 'split' ? f.buyerShareBps : d.status === 'disputed' ? previewBps : null;
  const buyerPart = splitBps != null ? Math.floor((d.amount * splitBps) / 10000) : 0;
  const sellerPart = d.amount - buyerPart;
  const isPreview = d.status === 'disputed' && previewBps != null;

  // What physically rests where, so the bond stacks on top of any principal.
  const principalAt = (s: Spot) =>
    splitBps == null ? f.principal === s : s === 'custody' ? isPreview : s === 'buyer' ? buyerPart > 0 : sellerPart > 0;
  const bondY = f.bond === 'none' ? 0
    : -(f.bond === 'custody' ? CUSTODY_LIFT : 0) - (principalAt(f.bond) ? PRINCIPAL_H : 0);
  const held = (principalAt('custody') ? PRINCIPAL_H : 0) + (f.bond === 'custody' ? BOND_H : 0);
  // The lid rests on whatever is held; with nothing inside it hangs open.
  const lidStyle = { ['--lid' as string]: `${SHELF + held + 2}px` } as CSSProperties;

  const vault =
    d.status === 'disputed' ? 'disputed'
    : d.status === 'failed' ? 'failed'
    : d.status === 'declined' || d.status === 'expired' || d.status === 'cancelled' ? 'void'
    : f.principal === 'custody' ? 'locked'
    : f.bond === 'custody' ? 'holding'
    : f.feeTaken ? 'released'
    : 'open';

  // Inspection countdown drains the ring around the seal.
  const windowSecs = policy.release_window_secs;
  const remaining = d.status === 'shipped' && d.release_after ? Math.max(0, d.release_after - now) : null;
  const RING = 2 * Math.PI * 21;
  const ringOffset = remaining == null ? RING : RING * (1 - remaining / windowSecs);

  const sealLabel =
    vault === 'disputed' ? 'Disputed'
    : remaining != null ? (remaining > 0 ? fmtDuration(Math.ceil(remaining)) : 'Due')
    : vault === 'locked' ? 'Locked'
    : vault === 'holding' ? 'Bond held'
    : vault === 'released' ? 'Settled'
    : d.status === 'failed' ? 'Failed'
    : d.status === 'expired' ? 'Lapsed'
    : d.status === 'declined' ? 'Declined'
    : d.status === 'cancelled' ? 'Void'
    : 'Open';

  const party = (addr: string | null, role: string) => (
    <div className="bridge-party">
      {addr && <Avatar a={addr} size={18} />}
      <span className="bridge-name">{addr ? nameOf(addr) : '—'}</span>
      <span className="bridge-role">{role}{addr && addr === active?.address ? ' · you' : ''}</span>
    </div>
  );

  return (
    <figure className="bridge" data-status={d.status} data-vault={vault} data-pending={pending || undefined}>
      <div className="bridge-stage" ref={stage} role="img" aria-label={fundsSentence(d, fmtAmount)}>
        <div className="bridge-floor" aria-hidden />

        {d.arbiter && (
          <div className="bridge-arbiter" aria-hidden>
            <span className="scales">⚖</span> {nameOf(d.arbiter)}
          </div>
        )}

        {(['buyer', 'custody', 'seller'] as const).map((s) => (
          <div key={s} className={`bridge-pad pad-${s}`} style={lane(X[s])} aria-hidden />
        ))}

        <div className="vault" aria-hidden>
          <div className="vault-back" />
          <div className="vault-treasury" title="Protocol treasury">treasury</div>
        </div>

        {/* Principal: one ingot, or two pieces when split / previewing a ruling. */}
        {splitBps == null ? (
          <div className="lane lane-principal" style={lane(X[f.principal as Spot], f.principal === 'custody' ? -CUSTODY_LIFT : 0)} aria-hidden>
            <div className={`ingot principal ${f.promised ? 'ghost' : ''} ${f.returned ? 'returned' : ''}`}>
              <span>{fmtAmount(f.principal === 'seller' ? d.amount - d.fee : d.amount)}</span>
            </div>
          </div>
        ) : (
          <>
            {isPreview && (
              <div className="lane lane-principal" style={lane(X.custody, -CUSTODY_LIFT)} aria-hidden>
                <div className="ingot principal"><span>{fmtAmount(d.amount)}</span></div>
              </div>
            )}
            {buyerPart > 0 && (
              <div className="lane lane-principal" style={lane(X.buyer)} aria-hidden>
                <div className={`ingot principal piece ${isPreview ? 'ghost' : ''}`} style={{ ['--w' as string]: splitBps! / 10000 }}>
                  <span>{fmtAmount(buyerPart)}</span>
                </div>
              </div>
            )}
            {sellerPart > 0 && (
              <div className="lane lane-principal" style={lane(X.seller)} aria-hidden>
                <div className={`ingot principal piece ${isPreview ? 'ghost' : ''}`} style={{ ['--w' as string]: 1 - splitBps! / 10000 }}>
                  <span>{fmtAmount(isPreview ? sellerPart : sellerPart - d.fee)}</span>
                </div>
              </div>
            )}
          </>
        )}

        {f.bond !== 'none' && (
          <div className="lane lane-bond" style={lane(X[f.bond], bondY)} aria-hidden>
            <div className="ingot bond"><span>bond {fmtAmount(d.bond)}</span></div>
          </div>
        )}

        {f.feeTaken && d.fee > 0 && (
          <div className="lane lane-fee" style={lane(X.custody)} aria-hidden>
            <div className="coin" title="Protocol fee">{fmtAmount(d.fee)}</div>
          </div>
        )}

        <div className="vault-lid" style={lidStyle} aria-hidden />
        <div className="vault-front" aria-hidden>
          <div className={`seal ${stampKey.current ? 'stamp' : ''}`} key={stampKey.current}>
            <svg viewBox="0 0 48 48" className="seal-ring">
              <circle cx="24" cy="24" r="21" className="ring-track" />
              {remaining != null && (
                <circle cx="24" cy="24" r="21" className="ring-fill" strokeDasharray={RING} strokeDashoffset={ringOffset} />
              )}
            </svg>
            <span className="seal-label">{sealLabel}</span>
          </div>
        </div>
      </div>

      <figcaption className="bridge-caption">
        {party(d.buyer, 'buyer')}
        <div className="bridge-party center">
          <span className="bridge-name">Custody</span>
          <span className="bridge-role">
            {pending ? 'signed · waiting for block' : isPreview ? 'previewing ruling' : sealLabel.toLowerCase()}
          </span>
        </div>
        {party(d.seller, 'seller')}
      </figcaption>
    </figure>
  );
}

/** Compact three-slot track for lists: where the money is, at a glance. */
export function MoneyTrack({ d }: { d: Deal }) {
  const f = fundsPosition(d);
  const where = f.principal === 'split' ? 'split between both' : f.principal === 'custody' ? 'in custody' : `with ${f.principal}`;
  return (
    <span className={`track track-${f.principal} ${f.promised || f.returned ? 'track-dim' : ''}`} title={`Funds ${where}`}>
      <span className="sr-only">Funds {where}</span>
      <i className="t-slot" aria-hidden /><i className="t-slot t-vault" aria-hidden /><i className="t-slot" aria-hidden />
      <b className="t-dot" aria-hidden />
      {f.principal === 'split' && <b className="t-dot t-dot-2" aria-hidden />}
    </span>
  );
}
