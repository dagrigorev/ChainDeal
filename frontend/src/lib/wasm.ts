// Typed façade over the Rust wallet compiled to WebAssembly (crates/wallet-wasm).
import init, * as w from '../wasm/chaindeal_wallet.js';
import type { Action, Block, DealPolicy, DealType, LineItem, PartyKind, SignedTx } from './types';

export interface Keypair { secret: string; pubkey: string; address: string }
export interface Quote { deal_type: DealType; amount: number; fee: number; bond: number; seller_payout: number; policy: DealPolicy }

let ready: Promise<unknown> | null = null;
export const initWasm = () => (ready ??= init());

export const generateWallet = (): Keypair => JSON.parse(w.generateWallet());
export const walletFromSecret = (secret: string): Keypair => JSON.parse(w.walletFromSecret(secret));
export const signAction = (secret: string, action: Action): SignedTx =>
  JSON.parse(w.signAction(secret, JSON.stringify(action), Date.now()));
/** Empty string when valid, otherwise the reason. */
export const verifyTx = (tx: SignedTx): string =>
  w.verifyTx(JSON.stringify({ hash: tx.hash, body: tx.body, signature: tx.signature }));
export const verifyBlock = (b: Block): string => w.verifyBlock(JSON.stringify(b));
export const classifyDeal = (seller: PartyKind, buyer: PartyKind) => w.classifyDeal(seller, buyer) as DealType;
export const quoteDeal = (t: DealType, items: LineItem[]): Quote => JSON.parse(w.quoteDeal(t, JSON.stringify(items)));
export const policies = (): DealPolicy[] => JSON.parse(w.policies());
export const isAddress = (s: string) => w.isAddress(s);
/** In-browser checks of a contract document: record hash, market signature, Merkle proofs. */
export const checkDocument = (doc: unknown): { hash_ok: boolean; signature_ok: boolean; events: { height: number; proof_ok: boolean }[]; number: string } =>
  JSON.parse(w.checkDocument(JSON.stringify(doc)));
/** QR code rendered by the Rust wallet (qrcodegen) as an SVG string. */
export const qrSvg = (text: string): string => w.qrSvg(text);
