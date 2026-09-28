// Proves to the auth service that this browser holds a wallet's private key:
// the service issues a one-time challenge, the Rust/WASM wallet signs it, the
// service verifies the ed25519 signature and binds the address to the account.
import { authApi } from './auth';
import { signLinkChallenge } from '../wasm/chaindeal_wallet.js';

export async function linkWallet(address: string, secret: string) {
  const ch = await authApi<{ challenge_id: string; message: string }>('/me/wallets/challenge', {
    method: 'POST',
    body: JSON.stringify({ address }),
  });
  const { pubkey, signature } = JSON.parse(signLinkChallenge(secret, ch.message));
  await authApi('/me/wallets', { method: 'POST', body: JSON.stringify({ challenge_id: ch.challenge_id, pubkey, signature }) });
}
