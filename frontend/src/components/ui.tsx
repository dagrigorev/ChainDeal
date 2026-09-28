import { useEffect, useState, type ReactNode } from 'react';
import { fmtAmount, short, STATUS_LABEL } from '../lib/format';
import { href } from '../lib/router';
import { useStore } from '../lib/store';
import type { DealStatus, DealType, PartyKind } from '../lib/types';

/** Fetches on mount, when deps change, and after every new block. */
export function useLive<T>(fn: () => Promise<T>, deps: unknown[] = []) {
  const { version } = useStore();
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let off = false;
    fn()
      .then((d) => !off && (setData(d), setError(null)))
      .catch((e) => !off && setError(e.message ?? String(e)));
    return () => {
      off = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, ...deps]);
  return { data, error, loading: data === null && error === null };
}

/** Re-renders every second (for countdowns and relative times). */
export function useNow() {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, []);
  return now;
}

export function StatusBadge({ status }: { status: DealStatus }) {
  return <span className={`badge st-${status}`}>{STATUS_LABEL[status]}</span>;
}

const KIND_WORD = { C: 'individual', B: 'business' } as const;

/**
 * Deal type as a pair of party glyphs, seller → buyer: ● individual, ■ business.
 * The shape carries the meaning, so it reads without color.
 */
export function TypeBadge({ type }: { type: DealType }) {
  const [s, , b] = type.split('') as ['B' | 'C', '2', 'B' | 'C'];
  return (
    <span className="dtype" title={`${KIND_WORD[s]} sells to ${KIND_WORD[b]}`}>
      <i className={`g g-${s}`} aria-hidden />
      <i className="g-arrow" aria-hidden />
      <i className={`g g-${b}`} aria-hidden />
      <b>{type}</b>
    </span>
  );
}

export function KindTag({ kind }: { kind: PartyKind }) {
  return (
    <span className={`kind kind-${kind}`}>
      <i className={`g g-${kind === 'business' ? 'B' : 'C'}`} aria-hidden />
      {kind === 'business' ? 'Business' : 'Individual'}
    </span>
  );
}

export function Amount({ v, unit = true, className = '' }: { v: number; unit?: boolean; className?: string }) {
  return (
    <span className={`amount ${className}`}>
      {fmtAmount(v)}
      {unit && <span className="unit"> DEAL</span>}
    </span>
  );
}

export function Addr({ a, you = true, link = true }: { a: string | null | undefined; you?: boolean; link?: boolean }) {
  const { nameOf, active } = useStore();
  if (!a) return <span className="muted">—</span>;
  const mine = you && active?.address === a;
  const inner = (
    <>
      <Avatar a={a} />
      <span>{nameOf(a)}</span>
      {mine && <span className="you">you</span>}
    </>
  );
  // link=false when rendered inside another link (nested <a> is invalid HTML).
  return link
    ? <a className="addr" href={href(`/accounts/${a}`)} title={a}>{inner}</a>
    : <span className="addr" title={a}>{inner}</span>;
}

/** Deterministic identicon-ish avatar from an address. */
export function Avatar({ a, size = 22 }: { a: string; size?: number }) {
  const h = parseInt(a.slice(2, 8), 16) || 0;
  const hue = h % 360;
  const hue2 = (hue + 40 + (h >> 9) % 80) % 360;
  return (
    <span
      className="avatar"
      style={{ width: size, height: size, background: `linear-gradient(135deg, hsl(${hue} 60% 52%), hsl(${hue2} 65% 42%))` }}
    />
  );
}

export function Hash({ h, to, n = 8 }: { h: string; to?: string; n?: number }) {
  const inner = <code className="hash">{short(h, n)}</code>;
  return to ? <a href={href(to)} title={h}>{inner}</a> : <span title={h}>{inner}</span>;
}

export function Copy({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className="btn ghost xs"
      onClick={() => {
        navigator.clipboard?.writeText(text).then(() => {
          setDone(true);
          setTimeout(() => setDone(false), 1200);
        });
      }}
    >
      {done ? 'Copied' : label}
    </button>
  );
}

export function Card({ title, actions, children, className = '' }: { title?: ReactNode; actions?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`card ${className}`}>
      {(title || actions) && (
        <header className="card-h">
          {title && <h3>{title}</h3>}
          {actions && <div className="card-actions">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty">{children}</div>;
}

export function Loading() {
  return <div className="loading"><span className="spinner" /> Loading…</div>;
}

export function ErrorNote({ msg }: { msg: string }) {
  return <div className="note err">{msg}</div>;
}

export function Stat({ label, value, sub }: { label: string; value: ReactNode; sub?: ReactNode }) {
  return (
    <div className="stat">
      <div className="stat-l">{label}</div>
      <div className="stat-v">{value}</div>
      {sub && <div className="stat-s">{sub}</div>}
    </div>
  );
}
