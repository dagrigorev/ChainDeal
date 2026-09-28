// OAuth 2.1 Authorization Code + PKCE client for the ChainDeal auth service.
//
// - The access token (5 min) lives only in memory — never in localStorage.
// - The refresh token is an HttpOnly, Secure, SameSite=Strict cookie scoped to
//   /oauth: JavaScript can't read it, so XSS can't exfiltrate a long-lived token.
// - state (CSRF), nonce (ID-token replay) and iss (RFC 9207 mix-up) are checked.
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';

export const CLIENT_ID = 'chaindeal-web';
const SCOPES = 'openid profile deals:write sim:control users:admin';
const PKCE_KEY = 'chaindeal.pkce';

export interface Claims {
  sub: string;
  name: string;
  roles: string[];
  wallets: string[];
  scope: string;
  sid: string;
  exp: number;
}

interface AuthCtx {
  ready: boolean;
  user: Claims | null;
  signIn(opts?: { returnTo?: string; prompt?: 'login' }): Promise<void>;
  signOut(): Promise<void>;
  /** Current access token, refreshed first if it is about to expire. */
  token(): Promise<string | null>;
  /** Force a refresh (e.g. after linking a wallet, so the token lists it). */
  refresh(): Promise<boolean>;
  hasRole(r: string): boolean;
  error: string | null;
}

const C = createContext<AuthCtx | null>(null);
export const useAuth = () => useContext(C)!;

// Module-level so non-React code (api.ts) can attach tokens.
let tokenGetter: () => Promise<string | null> = async () => null;
export const accessToken = () => tokenGetter();
/** Claims of the current access token (always fresh, unlike React state mid-update). */
export async function currentClaims(): Promise<Claims | null> {
  const t = await tokenGetter();
  return t ? (decode(t) as unknown as Claims) : null;
}

const b64url = (buf: ArrayBuffer | Uint8Array) =>
  btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const random = (n = 32) => b64url(crypto.getRandomValues(new Uint8Array(n)));

function decode(jwt: string): Record<string, unknown> {
  const p = jwt.split('.')[1].replace(/-/g, '+').replace(/_/g, '/');
  return JSON.parse(decodeURIComponent(escape(atob(p + '='.repeat((4 - (p.length % 4)) % 4)))));
}

async function tokenRequest(body: Record<string, string>) {
  const res = await fetch('/oauth/token', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams(body),
  });
  const json = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(json.error_description ?? json.error ?? 'token request failed');
  return json as { access_token: string; id_token?: string; expires_in: number };
}

let issuerCache: string | null = null;
// Sign-in completion must run once even though React dev mode double-invokes effects.
let booted = false;
async function issuer() {
  issuerCache ??= (await fetch('/.well-known/openid-configuration').then((r) => r.json())).issuer as string;
  return issuerCache;
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<Claims | null>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tok = useRef<string | null>(null);
  const timer = useRef<number | null>(null);
  const inflight = useRef<Promise<boolean> | null>(null);

  const accept = useCallback((access: string) => {
    tok.current = access;
    const c = decode(access) as unknown as Claims;
    setUser(c);
    if (timer.current) clearTimeout(timer.current);
    // Refresh a minute before expiry.
    const ms = Math.max(5_000, c.exp * 1000 - Date.now() - 60_000);
    timer.current = window.setTimeout(() => void refresh(), ms);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const clear = useCallback(() => {
    tok.current = null;
    setUser(null);
    if (timer.current) clearTimeout(timer.current);
  }, []);

  const refresh = useCallback(async (): Promise<boolean> => {
    // Coalesce concurrent refreshes: a rotated refresh token must be used once.
    inflight.current ??= tokenRequest({ grant_type: 'refresh_token', client_id: CLIENT_ID })
      .then((t) => (accept(t.access_token), true))
      .catch(() => (clear(), false))
      .finally(() => (inflight.current = null));
    return inflight.current;
  }, [accept, clear]);

  const token = useCallback(async () => {
    const c = tok.current ? (decode(tok.current) as unknown as Claims) : null;
    if (c && c.exp * 1000 - Date.now() > 30_000) return tok.current;
    if (c && (await refresh())) return tok.current;
    return null;
  }, [refresh]);

  useEffect(() => {
    tokenGetter = token;
  }, [token]);

  // On load: finish a redirect-back, or try a silent refresh from the cookie.
  useEffect(() => {
    if (booted) return;
    booted = true;
    (async () => {
      try {
        if (location.pathname === '/callback') {
          const q = new URLSearchParams(location.search);
          const saved = JSON.parse(sessionStorage.getItem(PKCE_KEY) ?? 'null');
          sessionStorage.removeItem(PKCE_KEY);
          // Drop the code from the address bar and history immediately.
          history.replaceState(null, '', '/#' + (saved?.returnTo ?? '/'));
          if (q.get('error')) throw new Error(q.get('error_description') ?? q.get('error')!);
          if (!saved || q.get('state') !== saved.state) throw new Error('Sign-in response did not match this browser (state mismatch).');
          if (q.get('iss') !== (await issuer())) throw new Error('Sign-in response came from an unexpected issuer.');
          const t = await tokenRequest({
            grant_type: 'authorization_code',
            code: q.get('code') ?? '',
            redirect_uri: location.origin + '/callback',
            client_id: CLIENT_ID,
            code_verifier: saved.verifier,
          });
          if (t.id_token && decode(t.id_token).nonce !== saved.nonce) throw new Error('ID token nonce mismatch.');
          accept(t.access_token);
          window.dispatchEvent(new HashChangeEvent('hashchange'));
        } else {
          await refresh();
        }
      } catch (e) {
        setError((e as Error).message);
        clear();
      } finally {
        setReady(true);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const signIn = useCallback(async (opts?: { returnTo?: string; prompt?: 'login' }) => {
    const verifier = random(48);
    const challenge = b64url(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier)));
    const state = random(16);
    const nonce = random(16);
    const returnTo = opts?.returnTo ?? (location.hash.replace(/^#/, '') || '/');
    sessionStorage.setItem(PKCE_KEY, JSON.stringify({ verifier, state, nonce, returnTo }));
    const q = new URLSearchParams({
      response_type: 'code',
      client_id: CLIENT_ID,
      redirect_uri: location.origin + '/callback',
      scope: SCOPES,
      state,
      nonce,
      code_challenge: challenge,
      code_challenge_method: 'S256',
    });
    if (opts?.prompt) q.set('prompt', opts.prompt);
    location.assign('/oauth/authorize?' + q);
  }, []);

  const signOut = useCallback(async () => {
    await fetch('/oauth/logout', { method: 'POST', credentials: 'same-origin' }).catch(() => {});
    clear();
  }, [clear]);

  const value = useMemo<AuthCtx>(
    () => ({ ready, user, signIn, signOut, token, refresh, error, hasRole: (r) => !!user?.roles.includes(r) }),
    [ready, user, signIn, signOut, token, refresh, error],
  );
  return <C.Provider value={value}>{children}</C.Provider>;
}

/** Calls the auth service's account API with the current access token. */
export async function authApi<T>(path: string, init: RequestInit = {}): Promise<T> {
  const t = await accessToken();
  if (!t) throw new Error('Please sign in again.');
  const res = await fetch('/oauth' + path, {
    ...init,
    headers: { ...(init.body ? { 'content-type': 'application/json' } : {}), ...init.headers, authorization: `Bearer ${t}` },
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error_description ?? body.error ?? res.statusText);
  return body as T;
}
