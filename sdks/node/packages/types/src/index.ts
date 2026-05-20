/**
 * x402 v2 wire-format types for the `miden-p2id-private` scheme.
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
export const MIDEN_MAINNET = 'miden:mainnet';

export type MidenNetwork = typeof MIDEN_TESTNET | typeof MIDEN_MAINNET;

// ---------- Identifiers ----------

/** Lowercase `0x`-prefixed hex string. */
export type HexId = string;

/** Decimal string of atomic units. */
export type DecimalAmount = string;

// ---------- Scheme types ----------

export const MIDEN_P2ID_PRIVATE_SCHEME = 'miden-p2id-private';
export type MidenP2idPrivateScheme = typeof MIDEN_P2ID_PRIVATE_SCHEME;

export interface MidenP2idPrivateExtra {
  /** Opaque tag the merchant uses to route incoming P2ID notes. */
  noteTag: string;
  /**
   * Server-issued 32-byte `serial_num` (canonical hex). Required when the
   * 402 is the result of a `POST /x402/challenge` round-trip; absent on the
   * initial bootstrap call before the merchant has obtained one.
   */
  serialNum?: HexId;
}

export interface MidenPaymentRequirements {
  scheme: MidenP2idPrivateScheme;
  network: MidenNetwork;
  amount: DecimalAmount;
  asset: HexId;
  payTo: HexId;
  maxTimeoutSeconds: number;
  extra: MidenP2idPrivateExtra;
}

/**
 * Wire payload for a `miden-p2id-private` payment.
 *
 * Carries a signed-but-unproven Miden transaction. The Guardian-facilitator
 * verifies the Falcon signature against one of the buyer's cosigner
 * commitments, reserves the input nullifiers, and asynchronously proves +
 * submits the tx via its batch worker. There is no `blockNum` because the
 * tx has not yet been included in a block at payload-construction time, and
 * there is no `transactionId` because the only meaningful id is the
 * post-prove `ProvenTransaction.id()` returned in the settle receipt.
 */
export interface MidenP2idPrivatePayload {
  noteType: typeof MIDEN_P2ID_PRIVATE_SCHEME;
  /** Base64-encoded canonical `miden_protocol::transaction::TransactionInputs`. */
  txInputs: string;
  /**
   * Base64-encoded `miden_protocol::account::auth::Signature` over the
   * `TransactionSummary::to_commitment()` digest.
   */
  signature: string;
  /**
   * Base64-encoded `miden_protocol::transaction::TransactionSummary` — the
   * value whose `.to_commitment()` the buyer signed. The facilitator binds
   * it to `txInputs` (via input-notes commitment) and to `expectedNoteBlob`
   * (via output-notes membership) before verifying the signature.
   */
  signedSummary: string;
  /** Base64-encoded `NoteFile::NoteDetails` for the expected output note. */
  expectedNoteBlob: string;
  /** Echo of `requirements.extra.serialNum`. */
  serialNum: HexId;
  sender: HexId;
  asset: HexId;
  amount: DecimalAmount;
}

/** Single-variant tagged wire payload — matches the Rust `MidenWirePayload`. */
export type MidenWirePayload = MidenP2idPrivatePayload;

// ---------- Top-level envelopes ----------

export interface ResourceInfo {
  url: string;
  description?: string;
  mimeType?: string;
}

export interface MidenPaymentRequired {
  x402Version: 2;
  /** Populated on a second 402 after a failed verify. */
  error?: string;
  resource?: ResourceInfo;
  accepts: MidenPaymentRequirements[];
  extensions?: Record<string, unknown>;
}

export interface MidenPaymentPayload {
  x402Version: 2;
  accepted: MidenPaymentRequirements;
  payload: MidenWirePayload;
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

/** Successful settle response from `POST /x402/settle`. */
export interface SettleSuccess {
  success: true;
  payer: HexId;
  /**
   * Deterministic queued id (`blake3(serial_num || tx_summary_commitment)`).
   * Resolves to the on-chain `ProvenTransaction.id()` once the batch worker
   * settles.
   */
  transaction: HexId;
  network: MidenNetwork;
  /** Base64-encoded Falcon-512 signature over `RPO256([payer, queuedId, network])`. */
  receiptSig: string;
  /** Hex commitment of the facilitator's receipt-signing pubkey. */
  receiptPubkeyCommitment: HexId;
}

export interface SettleError {
  success: false;
  errorReason: string;
  errorReasonDetails?: string;
}

export type SettleResponse = SettleSuccess | SettleError;

// ---------- Challenge endpoint ----------

export interface ChallengeRequest {
  paymentRequirements: MidenPaymentRequirements;
}

export interface ChallengeResponse {
  serialNum: HexId;
  expiresInSeconds: number;
}

// ---------- Pubkey endpoint ----------

export interface FacilitatorPubkey {
  /** Falcon-512 Poseidon2 pubkey commitment (canonical hex). */
  commitment: HexId;
  /** Base64 of the raw Falcon public-key bytes. */
  pubkeyB64: string;
}

// ---------- Error reasons ----------

/**
 * Canonical x402 `ErrorReason` values. The facilitator surfaces these
 * through `invalidReason`/`errorReason`; merchants forward them in the
 * `error` field of a follow-up 402.
 */
export const ERROR_REASONS = [
  'insufficient_funds',
  'invalid_scheme',
  'invalid_network',
  'invalid_payload',
  'invalid_transaction_state',
  'invalid_signature',
  'invalid_payment_amount',
  'unsupported_scheme',
  'unsupported_chain',
  'recipient_mismatch',
  'asset_mismatch',
  'unexpected_error',
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
