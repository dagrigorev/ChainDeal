import { useEffect, useState } from 'react';
import AccountPicker from '../components/AccountPicker';
import { Addr, Amount, Avatar, Card, Copy, Empty, KindTag } from '../components/ui';
import { api } from '../lib/api';
import { useAuth } from '../lib/auth';
import { short } from '../lib/format';
import { linkWallet } from '../lib/link';
import { fmtAmount, parseAmount } from '../lib/format';
import { useStore, type LocalWallet } from '../lib/store';
import type { PartyKind } from '../lib/types';
import { generateWallet, isAddress, walletFromSecret } from '../lib/wasm';

export default function WalletPage() {
  const { wallets, active, me, directory, setActive, addWallet, removeWallet, submit, busy, toast } = useStore();
  const [name, setName] = useState('');
  const [kind, setKind] = useState<PartyKind>('consumer');
  const [secret, setSecret] = useState('');
  const [reveal, setReveal] = useState<string | null>(null);
  const [demo, setDemo] = useState<(LocalWallet & { name: string })[] | null>(null);

  useEffect(() => {
    fetch('/demo-wallets.json')
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => Array.isArray(d) && setDemo(d))
      .catch(() => {});
  }, []);

  const auth = useAuth();
  const [linking, setLinking] = useState<string | null>(null);

  /** Proves key ownership to the auth service, then refreshes the token so it lists the wallet. */
  const link = async (w: LocalWallet) => {
    setLinking(w.address);
    try {
      await linkWallet(w.address, w.secret);
      await auth.refresh();
      toast('ok', `Linked ${short(w.address)} to your account`);
      return true;
    } catch (e) {
      toast('err', `Linking failed: ${(e as Error).message}`);
      return false;
    } finally {
      setLinking(null);
    }
  };

  const create = async () => {
    if (!auth.user) return auth.signIn({ returnTo: '/wallet' });
    const kp = generateWallet();
    const w: LocalWallet = { ...kp, label: name.trim(), kind };
    addWallet(w);
    // Link first: the chain API only accepts transactions from wallets you own.
    if (!(await link(w))) return;
    try {
      await submit({ type: 'register', name: name.trim(), kind }, { wallet: w });
      setName('');
    } catch {
      /* toast already shown; keep the keypair so the user can retry */
    }
  };

  const importSecret = async () => {
    let kp;
    try {
      kp = walletFromSecret(secret);
    } catch (e) {
      toast('err', `Invalid secret key: ${(e as Error).message}`);
      return;
    }
    const [onChain] = await api.lookup([kp.address]).catch(() => []);
    addWallet({ ...kp, label: onChain?.name ?? 'Imported wallet', kind: onChain?.kind });
    setSecret('');
    toast('ok', onChain ? `Imported ${onChain.name}` : 'Imported — register it below to start dealing');
  };

  const importDemo = () => {
    demo?.forEach((d) => addWallet({ secret: d.secret, pubkey: d.pubkey, address: d.address, label: d.name, kind: d.kind }));
    toast('ok', `Imported ${demo?.length} demo wallets — switch between them in the top bar`);
  };

  const unregistered = active && !me;

  return (
    <div className="page">
      <h1>Wallet</h1>
      <p className="lead">
        Keys never leave this browser. Generation and ed25519 signing run in <b>Rust compiled to WebAssembly</b> — the exact
        code the node uses to verify you. Keep several wallets to play buyer, seller and arbiter side by side.
      </p>

      <div className="grid-2">
        <Card title="Create a new identity">
          <div className="form">
            <label>Display name<input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Jane Doe or Acme Ltd" maxLength={64} /></label>
            <div className="seg">
              {(['consumer', 'business'] as const).map((k) => (
                <button key={k} className={kind === k ? 'on' : ''} onClick={() => setKind(k)}>
                  {k === 'consumer' ? 'Individual' : 'Business'}
                </button>
              ))}
            </div>
            <p className="muted small">
              Your kind decides deal rules: two individuals trade <b>C2C</b>, a business selling to you is <b>B2C</b>, two businesses deal <b>B2B</b>.
              New accounts receive 10,000.00 DEAL from the demo faucet.
            </p>
            <button className="btn primary" disabled={!name.trim() || busy || !!linking} onClick={create}>
              {auth.user ? 'Generate keys, link & register' : 'Sign in to create an identity'}
            </button>
          </div>
        </Card>

        <Card title="Import">
          <div className="form">
            <label>Secret key (hex)<input value={secret} onChange={(e) => setSecret(e.target.value)} placeholder="64 hex characters" className="mono" /></label>
            <button className="btn" disabled={secret.trim().length !== 64} onClick={importSecret}>Import wallet</button>
            {demo && (
              <>
                <hr />
                <p className="muted small">The seed script left {demo.length} demo identities (individuals, businesses and an arbiter).</p>
                <button className="btn" onClick={importDemo}>Import demo wallets</button>
              </>
            )}
          </div>
        </Card>
      </div>

      {unregistered && active && (
        <div className="note warn">
          <b>{active.label}</b> is not registered on-chain yet.{' '}
          <button className="btn xs primary" disabled={busy} onClick={() => submit({ type: 'register', name: active.label || 'Anonymous', kind: active.kind ?? 'consumer' })}>
            Register now
          </button>
        </div>
      )}

      <Card title={`Your wallets (${wallets.length})`}>
        {wallets.length === 0 ? <Empty>No wallets yet.</Empty> : (
          <table className="table">
            <thead><tr><th>Identity</th><th>Address</th><th className="r">Balance</th><th className="r">Escrowed</th><th /></tr></thead>
            <tbody>
              {wallets.map((w) => {
                const acc = directory.get(w.address);
                const isActive = active?.address === w.address;
                return (
                  <tr key={w.address} className={isActive ? 'active-row' : ''}>
                    <td>
                      <div className="ident">
                        <Avatar a={w.address} />
                        <div>
                          <div>{acc?.name ?? w.label} {isActive && <span className="you">active</span>}</div>
                          {acc ? <KindTag kind={acc.kind} /> : <span className="muted small">not registered</span>}
                        </div>
                      </div>
                    </td>
                    <td><code className="hash">{w.address}</code> <Copy text={w.address} /></td>
                    <td className="r">{acc ? <Amount v={acc.balance} /> : '—'}</td>
                    <td className="r">{acc ? fmtAmount(acc.escrowed) : '—'}</td>
                    <td className="r nowrap">
                      {auth.user && (auth.user.wallets.includes(w.address)
                        ? <span className="tag ok" title="Linked to your account">linked</span>
                        : <button className="btn xs" disabled={linking === w.address} onClick={() => link(w)}>{linking === w.address ? 'Linking…' : 'Link'}</button>)}{' '}
                      {!isActive && <button className="btn xs" onClick={() => setActive(w.address)}>Use</button>}{' '}
                      <button className="btn xs ghost" onClick={() => setReveal(reveal === w.address ? null : w.address)}>
                        {reveal === w.address ? 'Hide key' : 'Show key'}
                      </button>{' '}
                      <button
                        className="btn xs ghost danger"
                        onClick={() => confirm(`Forget ${acc?.name ?? w.label} from this browser? Without the secret key the account is lost.`) && removeWallet(w.address)}
                      >
                        Forget
                      </button>
                      {reveal === w.address && (
                        <div className="secret"><code>{w.secret}</code> <Copy text={w.secret} /></div>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </Card>

      {me && <TransferForm />}
    </div>
  );
}

function TransferForm() {
  const { active, submit, busy, me } = useStore();
  const [to, setTo] = useState('');
  const [amount, setAmount] = useState('');
  const [memo, setMemo] = useState('');
  const minor = parseAmount(amount);
  const valid = isAddress(to) && minor > 0 && me != null && minor <= me.balance;

  return (
    <Card title="Send DEAL">
      <div className="form row">
        <div className="grow"><AccountPicker label="Recipient" value={to} onChange={setTo} exclude={active ? [active.address] : []} /></div>
        <label>Amount<input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="0.00" inputMode="decimal" /></label>
        <label className="grow">Memo<input value={memo} onChange={(e) => setMemo(e.target.value)} maxLength={140} /></label>
        <button
          className="btn primary"
          disabled={!valid || busy}
          onClick={() => submit({ type: 'transfer', to, amount: minor, memo }).then(() => (setAmount(''), setMemo(''))).catch(() => {})}
        >
          Send
        </button>
      </div>
      {to && <p className="muted small">To <Addr a={to} /></p>}
    </Card>
  );
}
