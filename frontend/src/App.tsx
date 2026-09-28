import { useEffect, useRef, useState } from 'react';
import { Amount, Avatar } from './components/ui';
import { href, useRoute } from './lib/router';
import { useAuth } from './lib/auth';
import { useStore } from './lib/store';
import Account from './pages/Account';
import Admin from './pages/Admin';
import Accounts, { AccountView } from './pages/Accounts';
import Dashboard from './pages/Dashboard';
import DealDetail from './pages/DealDetail';
import Deals from './pages/Deals';
import DocumentPage from './pages/Document';
import Verify from './pages/Verify';
import Explorer, { BlockView, TxView } from './pages/Explorer';
import Live from './pages/Live';
import NewDeal from './pages/NewDeal';
import WalletPage from './pages/Wallet';

const NAV = [
  ['', 'Desk'],
  ['deals', 'Deals'],
  ['live', 'Live'],
  ['explorer', 'Ledger'],
  ['accounts', 'Directory'],
  ['wallet', 'Wallet'],
] as const;

export default function App() {
  const route = useRoute();
  const { toasts, toast } = useStore();
  const auth = useAuth();
  const [section, a, b] = route;
  useEffect(() => {
    if (auth.error) toast('err', `Sign-in failed: ${auth.error}`);
  }, [auth.error, toast]);

  let page;
  if (section === 'deals' && a === 'new') page = <NewDeal />;
  else if (section === 'deals' && a && b === 'document') page = <DocumentPage id={a} />;
  else if (section === 'deals' && a) page = <DealDetail id={a} />;
  else if (section === 'verify') page = <Verify />;
  else if (section === 'deals') page = <Deals />;
  else if (section === 'explorer' && a === 'block' && b) page = <BlockView id={b} />;
  else if (section === 'explorer' && a === 'tx' && b) page = <TxView hash={b} />;
  else if (section === 'explorer') page = <Explorer />;
  else if (section === 'accounts' && a) page = <AccountView addr={a} />;
  else if (section === 'accounts') page = <Accounts />;
  else if (section === 'wallet') page = <WalletPage />;
  else if (section === 'live') page = <Live />;
  else if (section === 'account') page = <Account />;
  else if (section === 'admin') page = <Admin />;
  else page = <Dashboard />;

  return (
    <>
      <a className="skip" href="#main" onClick={(e) => { e.preventDefault(); document.getElementById('main')?.focus(); }}>
        Skip to content
      </a>
      <header className="topbar">
        <a className="brand" href={href('/')} aria-label="ChainDeal — desk">
          <Mark /> <span>ChainDeal</span>
        </a>
        <nav aria-label="Primary">
          {[...NAV, ...(auth.hasRole('admin') ? ([['admin', 'Admin']] as const) : [])].map(([path, label]) => {
            const on = (section ?? '') === path;
            return (
              <a key={path} href={href(`/${path}`)} className={on ? 'on' : ''} aria-current={on ? 'page' : undefined}>
                {label}
              </a>
            );
          })}
        </nav>
        <span className="grow" />
        <ChainStack />
        <WalletSwitcher />
        <UserMenu />
      </header>
      {/* Keyed by route so each navigation gets a short orienting entrance. */}
      <main id="main" tabIndex={-1} key={route.join('/')}>
        {page}
      </main>
      <footer className="footer">
        Rust node · Tarantool ledger · React + Rust/WASM wallet · demo network — tokens carry no value
      </footer>
      <div className="toasts" role="status" aria-live="polite">
        {toasts.map((t) => <div key={t.id} className={`toast ${t.tone}`}>{t.text}</div>)}
      </div>
    </>
  );
}

/** Brand mark: two halves held by a seal — custody between two parties. */
function Mark() {
  return (
    <svg className="mark" viewBox="0 0 32 32" aria-hidden>
      <rect x="2" y="9" width="11" height="14" rx="2" />
      <rect x="19" y="9" width="11" height="14" rx="5.5" />
      <circle cx="16" cy="16" r="6" className="mark-seal" />
    </svg>
  );
}

/** Chain height as a stack of sealed slabs; a new slab drops in per block. */
function ChainStack() {
  const { height, version } = useStore();
  return (
    <a className="chainstack" href={href(`/explorer/block/${height}`)} aria-label={`Latest block ${height}`}>
      <span className="slabs" aria-hidden>
        <i /><i /><i />
        {version > 0 && <i className="slab-new" key={version} />}
      </span>
      <span className="chainstack-h">#{height}</span>
    </a>
  );
}

function WalletSwitcher() {
  const { wallets, active, me, directory, setActive } = useStore();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    document.addEventListener('mousedown', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('mousedown', close);
      document.removeEventListener('keydown', esc);
    };
  }, [open]);

  if (!active) return <a className="btn primary sm" href={href('/wallet')}>Create wallet</a>;
  return (
    <div className="switcher" ref={ref}>
      <button className="switcher-btn" onClick={() => setOpen(!open)} aria-expanded={open} aria-haspopup="true">
        <Avatar a={active.address} />
        <span className="switcher-name">{me?.name ?? active.label}</span>
        {me && <Amount v={me.balance} className="small" />}
        <span className="caret" aria-hidden>▾</span>
      </button>
      {open && (
        <div className="menu">
          <div className="menu-h">At the counter as</div>
          {wallets.map((w) => {
            const acc = directory.get(w.address);
            return (
              <button key={w.address} className={w.address === active.address ? 'on' : ''} aria-pressed={w.address === active.address}
                onClick={() => (setActive(w.address), setOpen(false))}>
                <Avatar a={w.address} />
                <span className="grow">{acc?.name ?? w.label}<span className="muted tiny"> {acc?.kind ?? 'unregistered'}</span></span>
                {acc && <Amount v={acc.balance} unit={false} className="small" />}
              </button>
            );
          })}
          <a className="menu-f" href={href('/wallet')} onClick={() => setOpen(false)}>Manage wallets →</a>
        </div>
      )}
    </div>
  );
}

/** Signed-in identity (from the auth service), separate from the active wallet. */
function UserMenu() {
  const { ready, user, signIn, signOut } = useAuth();
  if (!ready) return null;
  if (!user) return <button className="btn sm" onClick={() => signIn()}>Sign in</button>;
  return (
    <span className="user-chip">
      <a href={href('/account')} title={`Signed in as ${user.name} (${user.roles.join(', ')})`} aria-label={`Account: ${user.name}`}>
        <span className="user-initial" aria-hidden>{user.name.slice(0, 1).toUpperCase()}</span>
      </a>
      <button className="btn xs ghost" onClick={() => signOut()}>Sign out</button>
    </span>
  );
}
