import { useEffect, useRef, useState } from 'react';
import Outcomes from '../components/Outcomes';
import { Amount, Card, Empty, Loading, StatusBadge, TypeBadge, useLive } from '../components/ui';
import { api, ApiError } from '../lib/api';
import { useAuth } from '../lib/auth';
import { STATUS_LABEL } from '../lib/format';
import { href } from '../lib/router';
import { useStore, type NodeEvent } from '../lib/store';
import type { ClusterInfo, DealStatus, Metrics, SimConfig, SimSnapshot, TarantoolInstance, Transition } from '../lib/types';

/* --------------------------------------------------------------------------
   The Floor: the deal state machine drawn as a map. Node counts are live
   network-wide totals; every transition sealed in a block flies as a particle
   from its source state to its destination while the next block is mined.
   -------------------------------------------------------------------------- */

type NodeId = DealStatus | 'new';
const W = 1000;
const H = 360;
const NODES: Record<NodeId, { x: number; y: number; label: string }> = {
  new: { x: 40, y: 160, label: 'New' },
  proposed: { x: 170, y: 160, label: 'Proposed' },
  accepted: { x: 330, y: 160, label: 'Accepted' },
  funded: { x: 490, y: 160, label: 'Funded' },
  shipped: { x: 650, y: 160, label: 'Delivered' },
  completed: { x: 880, y: 160, label: 'Completed' },
  disputed: { x: 650, y: 44, label: 'Disputed' },
  resolved: { x: 880, y: 44, label: 'Resolved' },
  declined: { x: 170, y: 300, label: 'Declined' },
  expired: { x: 330, y: 300, label: 'Expired' },
  cancelled: { x: 490, y: 300, label: 'Cancelled' },
  failed: { x: 650, y: 300, label: 'Failed' },
};
const EDGES: [NodeId, NodeId][] = [
  ['new', 'proposed'], ['proposed', 'accepted'], ['accepted', 'funded'], ['funded', 'shipped'], ['shipped', 'completed'],
  ['funded', 'disputed'], ['shipped', 'disputed'], ['disputed', 'resolved'],
  ['proposed', 'declined'], ['proposed', 'expired'], ['accepted', 'expired'], ['proposed', 'cancelled'],
  ['accepted', 'cancelled'], ['funded', 'cancelled'], ['funded', 'failed'],
];
const FAIL = new Set<string>(['declined', 'expired', 'cancelled', 'failed']);
const GOOD = new Set<string>(['completed', 'resolved']);

/** Quadratic curve between two nodes; horizontal happy-path edges stay straight. */
function curve(a: NodeId, b: NodeId) {
  const p = NODES[a], q = NODES[b];
  const mx = (p.x + q.x) / 2, my = (p.y + q.y) / 2;
  const bend = p.y === q.y ? 0 : 0.18;
  const cx = mx + (q.y - p.y) * bend, cy = my - (q.x - p.x) * bend;
  return { p, q, cx, cy, d: `M${p.x},${p.y} Q${cx},${cy} ${q.x},${q.y}` };
}

interface Particle { a: NodeId; b: NodeId; start: number; dur: number; tone: 'go' | 'good' | 'fail' }
interface Ring { x: number; y: number; start: number; tone: Particle['tone'] }

function useReducedMotion() {
  const [r, setR] = useState(() => matchMedia('(prefers-reduced-motion: reduce)').matches);
  useEffect(() => {
    const m = matchMedia('(prefers-reduced-motion: reduce)');
    const on = () => setR(m.matches);
    m.addEventListener('change', on);
    return () => m.removeEventListener('change', on);
  }, []);
  return r;
}

function Floor({ counts, recent }: { counts: Partial<Record<DealStatus, number>>; recent: Record<string, number> }) {
  const { subscribe } = useStore();
  const canvas = useRef<HTMLCanvasElement>(null);
  const particles = useRef<Particle[]>([]);
  const rings = useRef<Ring[]>([]);
  const still = useReducedMotion();

  // Spawn one particle per transition, spread across the block interval so
  // the floor flows continuously instead of pulsing every two seconds.
  useEffect(() => {
    if (still) return;
    return subscribe((e: NodeEvent) => {
      if (e.type !== 'block') return;
      const now = performance.now();
      for (const t of e.transitions.slice(0, 80)) {
        const a: NodeId = t.from ?? 'new';
        if (!NODES[a] || !NODES[t.to]) continue;
        particles.current.push({
          a, b: t.to, start: now + Math.random() * 1700, dur: 900 + Math.random() * 400,
          tone: FAIL.has(t.to) ? 'fail' : GOOD.has(t.to) ? 'good' : 'go',
        });
      }
      if (particles.current.length > 400) particles.current.splice(0, particles.current.length - 400);
    });
  }, [subscribe, still]);

  // Canvas loop: DPR-aware, paused when the tab is hidden, idle when empty.
  useEffect(() => {
    if (still) return;
    const el = canvas.current!;
    const ctx = el.getContext('2d')!;
    const css = getComputedStyle(el);
    const color = { go: css.getPropertyValue('--brass').trim(), good: css.getPropertyValue('--verdigris').trim(), fail: css.getPropertyValue('--seal').trim() };
    let frame = 0;
    const fit = () => {
      const r = el.getBoundingClientRect();
      el.width = Math.round(r.width * devicePixelRatio);
      el.height = Math.round(r.height * devicePixelRatio);
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
    const draw = (now: number) => {
      frame = requestAnimationFrame(draw);
      const sx = el.width / W, sy = el.height / H;
      ctx.clearRect(0, 0, el.width, el.height);
      const alive: Particle[] = [];
      for (const pt of particles.current) {
        const k = (now - pt.start) / pt.dur;
        if (k < 0) { alive.push(pt); continue; }
        if (k >= 1) {
          rings.current.push({ x: NODES[pt.b].x, y: NODES[pt.b].y, start: now, tone: pt.tone });
          continue;
        }
        alive.push(pt);
        const { p, q, cx, cy } = curve(pt.a, pt.b);
        ctx.fillStyle = color[pt.tone];
        // Short tail: a few earlier samples at falling opacity.
        for (let i = 4; i >= 0; i--) {
          const t = Math.max(0, ease(k) - i * 0.025);
          const u = 1 - t;
          const x = u * u * p.x + 2 * u * t * cx + t * t * q.x;
          const y = u * u * p.y + 2 * u * t * cy + t * t * q.y;
          ctx.globalAlpha = i === 0 ? 1 : 0.35 - i * 0.06;
          ctx.beginPath();
          ctx.arc(x * sx, y * sy, (i === 0 ? 4.5 : 3.5) * sx, 0, Math.PI * 2);
          ctx.fill();
        }
      }
      particles.current = alive;
      ctx.globalAlpha = 1;
      rings.current = rings.current.filter((r) => {
        const k = (now - r.start) / 500;
        if (k >= 1) return false;
        ctx.strokeStyle = color[r.tone];
        ctx.globalAlpha = 0.6 * (1 - k);
        ctx.lineWidth = 2 * sx;
        ctx.beginPath();
        ctx.ellipse(r.x * sx, r.y * sy, (62 + 18 * k) * sx, (28 + 10 * k) * sy, 0, 0, Math.PI * 2);
        ctx.stroke();
        return true;
      });
      ctx.globalAlpha = 1;
    };
    frame = requestAnimationFrame(draw);
    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
    };
  }, [still]);

  return (
    <div className="floor-wrap">
      <div className="floor-inner">
      <svg className="floor" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Deal state machine with live counts per state">
        {EDGES.map(([a, b]) => (
          <path key={`${a}-${b}`} d={curve(a, b).d} className={`edge ${FAIL.has(b) ? 'edge-fail' : GOOD.has(b) ? 'edge-good' : ''}`} />
        ))}
        {(Object.keys(NODES) as NodeId[]).map((id) => {
          const n = NODES[id];
          if (id === 'new') {
            return (
              <g key={id} className="fnode fnode-new">
                <circle cx={n.x} cy={n.y} r={9} />
                <text x={n.x} y={n.y + 30} className="fnode-label">NEW</text>
              </g>
            );
          }
          const tone = FAIL.has(id) ? 'fail' : GOOD.has(id) ? 'good' : 'go';
          const perMin = recent[id] ?? 0;
          return (
            <g key={id} className={`fnode fnode-${tone}`}>
              <rect x={n.x - 62} y={n.y - 28} width={124} height={56} rx={6} />
              <text x={n.x} y={n.y - 8} className="fnode-label">{n.label.toUpperCase()}</text>
              <text x={n.x} y={n.y + 15} className="fnode-count">{(counts[id as DealStatus] ?? 0).toLocaleString()}</text>
              {perMin > 0 && <text x={n.x + 58} y={n.y - 32} className="fnode-rate">+{perMin}/min</text>}
            </g>
          );
        })}
      </svg>
      <canvas ref={canvas} className="floor-particles" aria-hidden />
      </div>
    </div>
  );
}

/** Throughput chart: confirmed tx/s (area) vs refused + rejected (line), last 2 minutes. */
function Throughput({ m, target }: { m: Metrics; target: number }) {
  const s = m.series;
  const confirmed = s.map((x) => x[2]);
  const bad = s.map((x) => x[1] + x[3]);
  const max = Math.max(target * 1.6, ...confirmed, ...bad, 5);
  const cw = 600, ch = 120;
  const X = (i: number) => (i / (s.length - 1)) * cw;
  const Y = (v: number) => ch - (v / max) * ch;
  // Blocks land every ~2s, so smooth confirmed over a 4s window for readability.
  const smooth = confirmed.map((_, i) => confirmed.slice(Math.max(0, i - 3), i + 1).reduce((a, b) => a + b, 0) / Math.min(4, i + 1));
  const area = `M0,${ch} ` + smooth.map((v, i) => `L${X(i).toFixed(1)},${Y(v).toFixed(1)}`).join(' ') + ` L${cw},${ch} Z`;
  const line = bad.map((v, i) => `${i ? 'L' : 'M'}${X(i).toFixed(1)},${Y(v).toFixed(1)}`).join(' ');
  const last = (arr: number[], n = 10) => arr.slice(-n).reduce((a, b) => a + b, 0) / n;
  return (
    <div className="tp">
      <dl className="tp-nums">
        <div><dt>Confirmed</dt><dd>{last(confirmed).toFixed(1)}<small> tx/s</small></dd></div>
        <div><dt>Admitted</dt><dd>{last(s.map((x) => x[0])).toFixed(1)}<small> tx/s</small></dd></div>
        <div className="bad"><dt>Refused</dt><dd>{last(s.map((x) => x[1])).toFixed(1)}<small> /s</small></dd></div>
        <div className="bad"><dt>Rejected in block</dt><dd>{last(s.map((x) => x[3])).toFixed(1)}<small> /s</small></dd></div>
      </dl>
      <svg viewBox={`0 0 ${cw} ${ch}`} className="tp-chart" preserveAspectRatio="none" role="img"
        aria-label={`Throughput over the last two minutes: about ${last(confirmed).toFixed(1)} confirmed transactions per second`}>
        <line x1={0} x2={cw} y1={Y(target)} y2={Y(target)} className="tp-target" />
        <path d={area} className="tp-area" />
        <path d={line} className="tp-bad" />
      </svg>
      <div className="tp-axis muted tiny"><span>−2 min</span><span>target {target} tx/s</span><span>now</span></div>
    </div>
  );
}

function Controls({ sim, onChange }: { sim: SimSnapshot; onChange(p: Partial<SimConfig>): void }) {
  const c = sim.config;
  const { user, hasRole, signIn } = useAuth();
  const allowed = sim.control_role === 'anyone' || hasRole('operator') || hasRole('admin');
  if (!allowed) {
    return (
      <div className="controls unlock">
        <p className="muted small">
          Market controls need the <b>operator</b> role.{' '}
          {!user && <button className="btn xs" onClick={() => signIn({ returnTo: '/live' })}>Sign in</button>}
        </p>
      </div>
    );
  }
  return (
    <div className="controls">
      <button className={`btn ${c.running ? '' : 'primary'}`} onClick={() => onChange({ running: !c.running })} aria-pressed={c.running}>
        {c.running ? '❚❚ Pause market' : '▶ Run market'}
      </button>
      <label className="ctl">
        <span>Rate <b>{c.rate} tx/s</b></span>
        <input type="range" min={1} max={50} step={1} value={c.rate} onChange={(e) => onChange({ rate: +e.target.value })} />
      </label>
      <label className="ctl">
        <span>Failure pressure <b>{c.pressure.toFixed(1)}×</b></span>
        <input type="range" min={0} max={3} step={0.1} value={c.pressure} onChange={(e) => onChange({ pressure: +e.target.value })} />
      </label>
      <label className="ctl">
        <span>Bad clients <b>{Math.round(c.noise * 100)}%</b></span>
        <input type="range" min={0} max={0.2} step={0.01} value={c.noise} onChange={(e) => onChange({ noise: +e.target.value })} />
      </label>
    </div>
  );
}

export default function Live() {
  const { subscribe } = useStore();
  const stats = useLive(api.stats);
  const [sim, setSim] = useState<SimSnapshot | null>(null);
  const [metrics, setMetrics] = useState<Metrics | null>(null);
  const [tape, setTape] = useState<(Transition & { at: number; key: string })[]>([]);
  const arrivals = useRef<{ to: string; at: number }[]>([]);
  const [recent, setRecent] = useState<Record<string, number>>({});

  // Poll the simulator and per-second metrics (cheap, bounded payloads).
  useEffect(() => {
    let off = false;
    const tick = () => {
      api.sim().then((s) => !off && setSim(s)).catch(() => {});
      api.metrics().then((m) => !off && setMetrics(m)).catch(() => {});
      const cutoff = Date.now() - 60_000;
      arrivals.current = arrivals.current.filter((a) => a.at > cutoff);
      const r: Record<string, number> = {};
      for (const a of arrivals.current) r[a.to] = (r[a.to] ?? 0) + 1;
      if (!off) setRecent(r);
    };
    tick();
    const t = setInterval(tick, 1000);
    return () => {
      off = true;
      clearInterval(t);
    };
  }, []);

  useEffect(
    () =>
      subscribe((e) => {
        if (e.type !== 'block') return;
        const at = Date.now();
        for (const t of e.transitions) arrivals.current.push({ to: t.to, at });
        setTape((prev) => [...e.transitions.slice(0, 16).map((t, i) => ({ ...t, at, key: `${e.height}-${i}` })), ...prev].slice(0, 16));
      }),
    [subscribe],
  );

  const { toast } = useStore();
  const change = (p: Partial<SimConfig>) => {
    setSim((s) => (s ? { ...s, config: { ...s.config, ...p } } : s));
    api.setSim(p).then(setSim).catch((e) => {
      if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
        toast('err', e.message);
        api.sim().then(setSim).catch(() => {});
      }
    });
  };

  const s = stats.data;
  const failed60 = ['declined', 'expired', 'cancelled', 'failed'].reduce((n, k) => n + (recent[k] ?? 0), 0);
  const closed60 = failed60 + (recent.completed ?? 0) + (recent.resolved ?? 0);

  return (
    <div className="page live">
      <div className="page-h">
        <div>
          <span className="eyebrow"><span className="live-dot" aria-hidden /> {sim?.config.running ? 'Market open' : 'Market paused'}</span>
          <h1>Live market</h1>
          <p className="lead">
            {sim ? <>{sim.agents.toLocaleString()} agents open, advance, decline, dispute and abandon deals. Every move is a signed transaction through the same contract you use.</> : 'Connecting…'}
          </p>
        </div>
        {sim && <Controls sim={sim} onChange={change} />}
      </div>

      <section className="sheet flow" aria-labelledby="flow-h">
        <div className="card-h">
          <h3 id="flow-h">Deal flow</h3>
          <span className="muted small">Counts are live totals per state across the whole chain; moves from the last minute are shown as +N/min.</span>
        </div>
        {s ? <Floor counts={s.deals_by_status} recent={recent} /> : <Loading />}
      </section>

      <div className="grid-2">
        <Card title="Throughput">{metrics ? <Throughput m={metrics} target={sim?.config.rate ?? 10} /> : <Loading />}</Card>
        <Card title="Last minute">
          {closed60 === 0 ? <Empty>Waiting for deals to close…</Empty> : (
            <div className="minute">
              <p className="minute-big">
                <b>{Math.round((100 * failed60) / closed60)}%</b> of deals that closed in the last minute did not go through
                <span className="muted"> ({failed60} of {closed60})</span>
              </p>
              <ul className="minute-list">
                {(['completed', 'resolved', 'declined', 'expired', 'cancelled', 'failed'] as const).map((k) => (
                  <li key={k}><StatusBadge status={k} /> <b>{recent[k] ?? 0}</b></li>
                ))}
              </ul>
              {sim && sim.stats.recent_refusals.length > 0 && (
                <>
                  <h4 className="minute-h">Recently refused by the contract</h4>
                  <ul className="refusals">
                    {sim.stats.recent_refusals.slice(0, 5).map((r, i) => <li key={i}><code>{r}</code></li>)}
                  </ul>
                </>
              )}
            </div>
          )}
        </Card>
      </div>

      <div className="grid-main">
        <Card title="Tape">
          {tape.length === 0 ? <Empty>Waiting for the next block…</Empty> : (
            <ol className="tape-list">
              {tape.map((t) => (
                <li key={t.key} className={FAIL.has(t.to) ? 'bad' : GOOD.has(t.to) ? 'good' : ''}>
                  <a href={href(`/deals/${t.id}`)}>
                    <TypeBadge type={t.deal_type} />
                    <span className="grow ellipsis">{t.title}</span>
                    <span className="tape-move">{t.from ? STATUS_LABEL[t.from] : 'New'} → <StatusBadge status={t.to} /></span>
                    <Amount v={t.amount} unit={false} />
                  </a>
                </li>
              ))}
            </ol>
          )}
        </Card>
        <aside className="stack">
          {s && (
            <Card title={`All-time outcomes · ${s.deals.toLocaleString()} deals · ${s.txs.toLocaleString()} txs`}>
              <Outcomes stats={s} />
            </Card>
          )}
          <Cluster />
          {sim && (
            <Card title="Simulator">
              <dl className="kv">
                <dt>Agents registered</dt><dd>{sim.stats.registered.toLocaleString()} / {sim.agents.toLocaleString()}</dd>
                <dt>Deals it's driving</dt><dd>{sim.active.toLocaleString()}</dd>
                <dt>Opened this session</dt><dd>{sim.stats.deals_opened.toLocaleString()}</dd>
                <dt>Adopted at start</dt><dd>{sim.stats.adopted.toLocaleString()}</dd>
                <dt>Sent / refused</dt><dd>{sim.stats.sent.toLocaleString()} / {sim.stats.refused.toLocaleString()}</dd>
                <dt>Deliberately invalid</dt><dd>{sim.stats.noise_sent.toLocaleString()}</dd>
              </dl>
            </Card>
          )}
        </aside>
      </div>
    </div>
  );
}

const mb = (n: number) => `${(n / 2 ** 20).toFixed(0)} MB`;

function Instance({ t, role }: { t: TarantoolInstance; role: string }) {
  const lag = t.replication.map((r) => r.upstream?.lag).find((l) => l != null);
  return (
    <li className={`inst ${t.ro ? 'ro' : 'rw'}`}>
      <span className="inst-dot" aria-hidden />
      <span className="grow">
        <b>{t.hostname}</b> <span className="muted small">{role} · {t.ro ? 'read-only' : 'read-write'}</span>
        <span className="muted tiny inst-meta">
          lsn {t.lsn.toLocaleString()} · {mb(t.memory_used)} / {mb(t.memory_limit)} · {t.txs.toLocaleString()} txs
          {lag != null && <> · lag {(lag * 1000).toFixed(1)} ms</>}
        </span>
      </span>
      <span className={`tag ${t.status === 'running' ? 'ok' : 'warn'}`}>{t.status}</span>
    </li>
  );
}

/** Live topology: API nodes (with the lease holder) and the Tarantool replica set. */
function Cluster() {
  const [c, setC] = useState<ClusterInfo | null>(null);
  useEffect(() => {
    let off = false;
    const tick = () => api.cluster().then((v) => !off && setC(v)).catch(() => {});
    tick();
    const t = setInterval(tick, 3000);
    return () => {
      off = true;
      clearInterval(t);
    };
  }, []);
  if (!c) return null;
  const nodes = [...c.nodes].sort((a, b) => a.id.localeCompare(b.id));
  return (
    <Card title={`Cluster · ${nodes.length} node${nodes.length === 1 ? '' : 's'}`}>
      <ul className="cluster-list" aria-label="API nodes">
        {nodes.map((n) => {
          const leader = n.id === c.leader;
          return (
            <li key={n.id} className={leader ? 'leader' : ''}>
              <span className="inst-dot" aria-hidden />
              {/* Pod names are long (backend-<rs-hash>-<pod-hash>): show the unique suffix. */}
              <span className="grow ellipsis" title={n.id}><b>{n.id.split('-')[0]}</b> <span className="mono small muted">{n.id.split('-').pop()}</span>{n.id === c.serving_node && <span className="you">serving you</span>}</span>
              <span className={`tag ${leader ? 'warn' : 'ok'}`}>{leader ? 'leader · mining' : 'follower'}</span>
            </li>
          );
        })}
      </ul>
      <h4 className="minute-h">Tarantool replica set</h4>
      <ul className="cluster-list">
        <Instance t={c.master} role="master" />
        {c.read_replica && c.read_replica.uuid !== undefined && c.read_replica.hostname !== c.master.hostname && (
          <Instance t={c.read_replica} role="read pool" />
        )}
      </ul>
      {c.read_replica?.hostname === c.master.hostname && (
        <p className="muted tiny">This node's read connection currently lands on the master.</p>
      )}
    </Card>
  );
}
