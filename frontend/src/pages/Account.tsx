import { useCallback, useEffect, useState } from 'react';
import { Addr, Card, Empty, Loading } from '../components/ui';
import { authApi, useAuth } from '../lib/auth';
import { fmtTime, timeAgo } from '../lib/format';
import { useStore } from '../lib/store';

export interface UserView {
  id: string;
  email: string;
  name: string;
  roles: string[];
  status: string;
  created_at: number;
  failed_logins?: number;
  locked_until?: number;
  wallets: { address: string; linked_at: number }[];
  sessions?: { id: string; created_at: number; ip: string; ua: string; current: boolean }[];
  scope?: string;
}

export function RoleTags({ roles }: { roles: string[] }) {
  return (
    <span className="row gap">
      {roles.map((r) => <span key={r} className={`role-tag role-${r}`}>{r}</span>)}
    </span>
  );
}

const device = (ua: string) =>
  /iPhone|iPad/.test(ua) ? 'iOS' : /Android/.test(ua) ? 'Android' : /Mac OS/.test(ua) ? 'macOS' : /Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : 'Unknown device';
const browser = (ua: string) =>
  /Edg\//.test(ua) ? 'Edge' : /Chrome\//.test(ua) ? 'Chrome' : /Firefox\//.test(ua) ? 'Firefox' : /Safari\//.test(ua) ? 'Safari' : /curl/.test(ua) ? 'curl' : 'browser';

export default function Account() {
  const auth = useAuth();
  const { toast } = useStore();
  const [me, setMe] = useState<UserView | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [cur, setCur] = useState('');
  const [next, setNext] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => authApi<UserView>('/me').then(setMe).catch((e) => setErr(e.message)), []);
  useEffect(() => {
    if (auth.user) load();
  }, [auth.user, load]);

  if (!auth.ready) return <div className="page"><Loading /></div>;
  if (!auth.user) {
    return (
      <div className="page">
        <h1>Account</h1>
        <Card><Empty>You're signed out. <button className="btn xs primary" onClick={() => auth.signIn({ returnTo: '/account' })}>Sign in</button></Empty></Card>
      </div>
    );
  }
  if (!me) return <div className="page">{err ? <div className="note err">{err}</div> : <Loading />}</div>;

  const act = async (fn: () => Promise<unknown>, ok: string) => {
    setBusy(true);
    try {
      await fn();
      toast('ok', ok);
      await load();
    } catch (e) {
      toast('err', (e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page">
      <section className="counter" aria-label="Profile">
        <div className="counter-id">
          <span className="eyebrow">Account</span>
          <h1>{me.name}</h1>
          <p className="muted">{me.email} · member since {fmtTime(me.created_at)}</p>
          <RoleTags roles={me.roles} />
        </div>
        <button className="btn" onClick={() => auth.signOut()}>Sign out</button>
      </section>

      <div className="grid-2">
        <Card title="Linked wallets">
          <p className="muted small">Only wallets linked here can send transactions for your account. Linking proves you hold the key by signing a one-time challenge in your browser.</p>
          {me.wallets.length === 0 ? <Empty>No wallets linked yet — use <a href="#/wallet">Wallet</a> → Link.</Empty> : (
            <ul className="list">
              {me.wallets.map((w) => (
                <li key={w.address} className="row-link">
                  <Addr a={w.address} />
                  <span className="grow muted small">linked {timeAgo(w.linked_at)}</span>
                  <button className="btn xs ghost danger" disabled={busy}
                    onClick={() => confirm('Unlink this wallet? It will no longer be able to transact for your account.') &&
                      act(async () => { await authApi(`/me/wallets/${w.address}`, { method: 'DELETE' }); await auth.refresh(); }, 'Wallet unlinked')}>
                    Unlink
                  </button>
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card title="Change password">
          <form className="form" onSubmit={(e) => {
            e.preventDefault();
            act(() => authApi('/me/password', { method: 'POST', body: JSON.stringify({ current: cur, new: next }) }), 'Password changed; other sessions were signed out')
              .then(() => { setCur(''); setNext(''); });
          }}>
            <label>Current password<input type="password" autoComplete="current-password" value={cur} onChange={(e) => setCur(e.target.value)} required /></label>
            <label>New password<input type="password" autoComplete="new-password" minLength={12} value={next} onChange={(e) => setNext(e.target.value)} required /></label>
            <p className="muted tiny">At least 12 characters. Changing it signs out every other device.</p>
            <button className="btn primary" disabled={busy || !cur || next.length < 12}>Change password</button>
          </form>
        </Card>
      </div>

      <Card title="Active sessions" actions={
        <button className="btn xs ghost danger" disabled={busy}
          onClick={() => confirm('Sign out on every device, including this one?') && act(async () => { await authApi('/me/logout-all', { method: 'POST' }); await auth.signOut(); }, 'Signed out everywhere')}>
          Sign out everywhere
        </button>
      }>
        <table className="table">
          <thead><tr><th scope="col">Device</th><th scope="col">IP</th><th scope="col">Signed in</th><th /></tr></thead>
          <tbody>
            {(me.sessions ?? []).map((s) => (
              <tr key={s.id}>
                <td>{browser(s.ua)} on {device(s.ua)} {s.current && <span className="you">this device</span>}</td>
                <td className="mono small">{s.ip}</td>
                <td className="muted small">{timeAgo(s.created_at)}</td>
                <td className="r">
                  {!s.current && (
                    <button className="btn xs ghost" disabled={busy} onClick={() => act(() => authApi(`/me/sessions/${s.id}`, { method: 'DELETE' }), 'Session revoked')}>Revoke</button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="muted tiny">Revoking a session invalidates its refresh token immediately; any access token it holds expires within 5 minutes.</p>
      </Card>
    </div>
  );
}
