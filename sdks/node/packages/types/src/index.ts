/**
 * x402 v2 wire-format types for the Miden `exact` scheme.
 *
 * Mirrors the Rust crate `miden-x402-types`. JSON keys use camelCase and
 * match `docs/protocol.md` of the parent repo. Header constants and the
 * base64 codecs match `crates/miden-x402-types/src/header.rs`.
 *
 * Ports of this SDK to other languages should follow `docs/protocol.md`
 * as the normative spec; this file is purely a TypeScript projection of it.
 */

// ---------- Header names ----------

export const PAYMENT_REQUIRED_HEADER = 'Payment-Required';
export const PAYMENT_SIGNATURE_HEADER = 'Payment-Signature';
export const PAYMENT_RESPONSE_HEADER = 'Payment-Response';

// ---------- Network ----------

export const MIDEN_TESTNET = 'miden:testnet';
export const MIDEN_MAINNET = 'miden:mainnet'; // reserved, unused in MVP

export type MidenNetwork = typeof MIDEN_TESTNET | typeof MIDEN_MAINNET;

// ---------- Identifiers ----------

/** Lowercase `0x`-prefixed hex string. */
export type HexId = string;

/** Decimal string of atomic units. */
export type DecimalAmount = string;

// ---------- Scheme types ----------

export const EXACT_SCHEME = 'exact';
export type ExactScheme = typeof EXACT_SCHEME;

export const ASSET_TRANSFER_METHOD_P2ID = 'miden-p2id';
export type AssetTransferMethodTag = typeof ASSET_TRANSFER_METHOD_P2ID;

export type NoteKind = 'public' | 'private';

/**
 * Settlement model. `commit` (default) = settled-at-commit; `guardian-fast`
 * = verify-before-prove via the Guardian facilitator endpoints.
 */
export type SettlementKind = 'commit' | 'guardian-fast';

export interface MidenExactExtra {
  assetTransferMethod: AssetTransferMethodTag;
  tokenSymbol: string;
  decimals: number;
  noteType: NoteKind;
  /** Optional. Defaults to `'commit'` when absent. */
  settlement?: SettlementKind;
  /** Required when `settlement: 'guardian-fast'`. URL of the Guardian facilitator. */
  guardianUrl?: string;
  /** Required when `settlement: 'guardian-fast'`. Server-generated 32-byte hex word. */
  serialNum?: HexId;
}

export interface MidenPaymentRequirements {
  scheme: ExactScheme;
  network: MidenNetwork;
  amount: DecimalAmount;
  asset: HexId;
  payTo: HexId;
  maxTimeoutSeconds: number;
  extra: MidenExactExtra;
}

export interface PublicP2idPayload {
  noteType: 'public';
  noteId: HexId;
  transactionId: HexId;
  sender: HexId;
  blockNum: number;
  asset: HexId;
  amount: DecimalAmount;
}

/**
 * Private-note payment payload.
 *
 * Carries the canonical Miden `NoteFile` (base64-encoded) so the facilitator
 * can reconstruct the note off-chain and bind it to the on-chain commitment
 * by recomputing the note id. The remaining fields mirror
 * {@link PublicP2idPayload} so the wire envelope is uniform across both note
 * types and the merchant can produce the same `SettleResponse::Success`
 * shape regardless of `noteType`.
 */
export interface PrivateP2idPayload {
  noteType: 'private';
  /** Base64-encoded canonical NoteFile blob. */
  noteBlob: string;
  transactionId: HexId;
  sender: HexId;
  blockNum: number;
  asset: HexId;
  amount: DecimalAmount;
}

/**
 * Guardian-fast payment payload.
 *
 * Carries a signed-but-unproven Miden transaction. The facilitator verifies
 * the Falcon signature offline, reserves the input nullifiers, and proves +
 * submits the tx asynchronously. Unlike the commit variants, there is no
 * `blockNum` because the tx has not yet been included in a block at
 * payload-construction time, and `transactionId` is the pre-prove id —
 * the post-prove id is the one returned by `/guardian/settle`.
 */
export interface GuardianFastPayload {
  noteType: 'guardianFast';
  /** Base64-encoded canonical TransactionInputs. */
  txInputs: string;
  /**
   * Base64-encoded `miden_protocol::account::auth::Signature` over the
   * `TransactionSummary::to_commitment()` digest. Carried as a separate
   * field because the advice-map encoding inside TransactionInputs is the
   * stack-reversed prepared form, which can't be inverted back to the
   * high-level Signature needed for offline verification.
   */
  signature: string;
  /**
   * Base64-encoded `miden_protocol::transaction::TransactionSummary` — the
   * value whose `.toCommitment()` the buyer signed. The Guardian binds it
   * to `txInputs` (via input-notes commitment) and to `expectedNoteBlob`
   * (via output-notes commitment) before verifying the signature.
   */
  signedSummary: string;
  /** Base64-encoded NoteFile::NoteDetails for the expected output note. */
  expectedNoteBlob: string;
  /** Echo of `requirements.extra.serialNum`. */
  serialNum: HexId;
  transactionId: HexId;
  sender: HexId;
  asset: HexId;
  amount: DecimalAmount;
}

export type MidenExactPayload = PublicP2idPayload | PrivateP2idPayload | GuardianFastPayload;

// ---------- Top-level envelopes ----------

export interface ResourceInfo {
  url: string;
  description?: string;
  mimeType?: string;
}

export interface MidenPaymentRequired {
  x402Version: 2;
  /** Populated on the second 402 after a failed verify (e.g. "note already consumed"). */
  error?: string;
  resource?: ResourceInfo;
  accepts: MidenPaymentRequirements[];
  extensions?: Record<string, unknown>;
}

export interface MidenPaymentPayload {
  x402Version: 2;
  accepted: MidenPaymentRequirements;
  payload: MidenExactPayload;
  resource?: ResourceInfo;
  extensions?: Record<string, unknown>;
}

// ---------- Facilitator request / response bodies ----------

export interface MidenVerifyRequest {
  x402Version: 2;
  paymentPayload: MidenPaymentPayload;
  paymentRequirements: MidenPaymentRequirements;
}

export type MidenSettleRequest = MidenVerifyRequest;

export type VerifyResponse =
  | { isValid: true; payer: HexId }
  | { isValid: false; invalidReason: string; invalidReasonDetails?: string };

export interface SettleSuccess {
  success: true;
  payer: HexId;
  transaction: HexId;
  network: MidenNetwork;
}

export interface SettleError {
  success: false;
  errorReason: string;
  errorReasonDetails?: string;
}

export type SettleResponse = SettleSuccess | SettleError;

/**
 * Canonical x402 `ErrorReason` values. The facilitator surfaces these
 * through `invalidReason` / `errorReason`; merchants forward them in the
 * `error` field of the second 402.
 */
export const ERROR_REASONS = [
  'insufficient_funds',
  'invalid_scheme',
  'invalid_network',
  'invalid_payload',
  'invalid_transaction_state',
  'unsupported_scheme',
  'unexpected_verify_error',
  'unexpected_settle_error',
] as const;

export type ErrorReason = (typeof ERROR_REASONS)[number];

// ---------- Base64 header codecs ----------

const utf8 = new TextEncoder();
const utf8d = new TextDecoder();

function bytesToBase64(bytes: Uint8Array): string {
  if (typeof Buffer !== 'undefined') {
    return Buffer.from(bytes).toString('base64');
  }
  let s = '';
  for (let i = 0; i < bytes.length; i++) {
    s += String.fromCharCode(bytes[i]!);
  }
  return btoa(s);
}

function base64ToBytes(b64: string): Uint8Array {
  if (typeof Buffer !== 'undefined') {
    return new Uint8Array(Buffer.from(b64, 'base64'));
  }
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    out[i] = bin.charCodeAt(i);
  }
  return out;
}

export function encodeHeader<T>(value: T): string {
  return bytesToBase64(utf8.encode(JSON.stringify(value)));
}

export function decodeHeader<T>(value: string): T {
  return JSON.parse(utf8d.decode(base64ToBytes(value))) as T;
}

export const encodePaymentRequiredHeader = (v: MidenPaymentRequired) => encodeHeader(v);
export const decodePaymentRequiredHeader = (s: string) => decodeHeader<MidenPaymentRequired>(s);
export const encodePaymentSignatureHeader = (v: MidenPaymentPayload) => encodeHeader(v);
export const decodePaymentSignatureHeader = (s: string) => decodeHeader<MidenPaymentPayload>(s);
export const encodePaymentResponseHeader = (v: SettleResponse) => encodeHeader(v);
export const decodePaymentResponseHeader = (s: string) => decodeHeader<SettleResponse>(s);
