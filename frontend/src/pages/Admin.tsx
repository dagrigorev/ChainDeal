import { useCallback, useEffect, useState } from 'react';
import { Card, Empty, Loading } from '../components/ui';
import { authApi, useAuth } from '../lib/auth';
import { fmtTime, timeAgo } from '../lib/format';
import { useStore } from '../lib/store';
import type { UserView } from './Account';

interface AuditEvent { seq: number; at: number; event: string; user_id: string | null; ip: string; detail: Record<string, unknown> }

const ROLES = ['user', 'operator', 'admin'] as const;
const PAGE = 25;
const RISKY = new Set(['admin_user_deleted', 'login_failed', 'login_blocked', 'refresh_reuse_detected', 'pkce_failed', 'client_auth_failed', 'wallet_link_failed', 'password_change_failed']);

export default function Admin() {
  const auth = useAuth();
  const { toast } = useStore();
  const [users, setUsers] = useState<{ total: number; items: UserView[] } | null>(null);
  const [audit, setAudit] = useState<AuditEvent[] | null>(null);
  const [email, setEmail] = useState('');
  const [page, setPage] = useState(0);
  const [focus, setFocus] = useState<string>('');
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const q = email.trim() ? `email=${encodeURIComponent(email.trim())}` : `offset=${page * PAGE}&limit=${PAGE}`;
      setUsers(await authApi(`/admin/users?${q}`));
      setAudit((await authApi<{ items: AuditEvent[] }>(`/admin/audit?limit=60${focus ? `&user_id=${focus}` : ''}`)).items);
      setErr(null);
    } catch (e) {
      setErr((e as Error).message);
    }
  }, [email, page, focus]);

  useEffect(() => {
    if (auth.hasRole('admin')) load();
  }, [auth, load]);

  if (!auth.ready) return <div className="page"><Loading /></div>;
  if (!auth.hasRole('admin')) {
    return <div className="page"><h1>Administration</h1><Card><Empty>This area requires the <b>admin</b> role.</Empty></Card></div>;
  }

  const update = async (u: UserView, patch: Record<string, unknown>, msg: string) => {
    try {
      await authApi(`/admin/users/${u.id}`, { method: 'PATCH', body: JSON.stringify(patch) });
      toast('ok', msg);
      load();
    } catch (e) {
      toast('err', (e as Error).message);
    }
  };
  const nameOf = (id: string | null) => users?.items.find((u) => u.id === id)?.name ?? (id ? id.slice(0, 8) : '—');

  return (
    <div className="page">
      <div className="page-h">
        <div>
          <h1>Administration</h1>
          <p className="lead">Users, roles and the security audit log. Role changes and suspensions sign the user out everywhere, so removed privileges can't linger in old tokens.</p>
        </div>
      </div>
      {err && <div className="note err">{err}</div>}

      <Card title={`Users${users ? ` · ${users.total}` : ''}`} actions={
        <form className="row gap" onSubmit={(e) => { e.preventDefault(); setPage(0); load(); }}>
          <label className="sr-only" htmlFor="adm-email">Find by exact email</label>
          <input id="adm-email" className="compact-input" type="email" placeholder="Exact email (encrypted at rest)" value={email} onChange={(e) => setEmail(e.target.value)} />
          <button className="btn xs">Find</button>
        </form>
      }>
        {!users ? <Loading /> : users.items.length === 0 ? <Empty>No users.</Empty> : (
          <table className="table">
            <thead><tr><th scope="col">User</th><th scope="col">Roles</th><th scope="col">Wallets</th><th scope="col">Status</th><th scope="col" className="r">Actions</th></tr></thead>
            <tbody>
              {users.items.map((u) => {
                const self = u.id === auth.user?.sub;
                const locked = (u.locked_until ?? 0) * 1000 > Date.now();
                return (
                  <tr key={u.id} className={focus === u.id ? 'active-row' : ''}>
                    <td>
                      <b>{u.name}</b> {self && <span className="you">you</span>}
                      <div className="muted small">{u.email} · joined {timeAgo(u.created_at)}</div>
                    </td>
                    <td>
                      <fieldset className="role-edit">
                        <legend className="sr-only">Roles for {u.name}</legend>
                        {ROLES.map((r) => (
                          <label key={r} className="role-check">
                            <input type="checkbox" checked={u.roles.includes(r)} disabled={r === 'user' || (self && r === 'admin')}
                              onChange={(e) => update(u, { roles: e.target.checked ? [...u.roles, r] : u.roles.filter((x) => x !== r) }, `Roles updated for ${u.name}`)} />
                            {r}
                          </label>
                        ))}
                      </fieldset>
                    </td>
                    <td>{u.wallets.length}</td>
                    <td>
                      <span className={`tag ${u.status === 'active' ? 'ok' : 'err'}`}>{u.status}</span>
                      {locked && <span className="tag warn" title={`until ${fmtTime(u.locked_until!)}`}> locked</span>}
                      {(u.failed_logins ?? 0) > 0 && <div className="muted tiny">{u.failed_logins} failed logins</div>}
                    </td>
                    <td className="r nowrap">
                      {locked && <button className="btn xs" onClick={() => update(u, { unlock: true }, 'Account unlocked')}>Unlock</button>}{' '}
                      {!self && (u.status === 'active'
                        ? <button className="btn xs ghost danger" onClick={() => confirm(`Suspend ${u.name}? They are signed out everywhere.`) && update(u, { status: 'disabled' }, `${u.name} suspended`)}>Suspend</button>
                        : <button className="btn xs" onClick={() => update(u, { status: 'active' }, `${u.name} reactivated`)}>Reactivate</button>)}{' '}
                      <button className="btn xs ghost" onClick={() => setFocus(focus === u.id ? '' : u.id)}>{focus === u.id ? 'All events' : 'Events'}</button>{' '}
                      {!self && (
                        <button className="btn xs ghost danger"
                          onClick={async () => {
                            if (!confirm(`Delete ${u.name} permanently? Sessions end, wallet links are released; the audit trail is kept.`)) return;
                            try {
                              await authApi(`/admin/users/${u.id}`, { method: 'DELETE' });
                              toast('ok', `${u.name} deleted`);
                              load();
                            } catch (e) {
                              toast('err', (e as Error).message);
                            }
                          }}>
                          Delete
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {users && !email && users.total > PAGE && (
          <div className="pager">
            <button className="btn xs" disabled={page === 0} onClick={() => setPage(page - 1)}>← Previous</button>
            <span className="muted small">Page {page + 1} of {Math.ceil(users.total / PAGE)}</span>
            <button className="btn xs" disabled={(page + 1) * PAGE >= users.total} onClick={() => setPage(page + 1)}>Next →</button>
          </div>
        )}
      </Card>

      <Card title={focus ? `Audit log · ${nameOf(focus)}` : 'Audit log'} actions={<button className="btn xs" onClick={load}>Refresh</button>}>
        {!audit ? <Loading /> : audit.length === 0 ? <Empty>No events.</Empty> : (
          <ol className="audit">
            {audit.map((e) => (
              <li key={e.seq} className={RISKY.has(e.event) ? 'risky' : ''}>
                <span className="mono tiny muted">{fmtTime(e.at)}</span>
                <b>{e.event.replace(/_/g, ' ')}</b>
                <span className="small">{e.user_id ? nameOf(e.user_id) : 'anonymous'}</span>
                <span className="mono tiny muted">{e.ip}</span>
                {Object.keys(e.detail ?? {}).length > 0 && <code className="tiny">{JSON.stringify(e.detail)}</code>}
              </li>
            ))}
          </ol>
        )}
      </Card>
    </div>
  );
}
