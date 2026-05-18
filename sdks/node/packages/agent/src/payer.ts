/**
 * Abstract `Payer` interface: anything that can build, prove, and submit a
 * P2ID note on Miden testnet (public or private), and wait for it to land
 * in a committed block.
 *
 * The reference implementation in `wasm-sdk-payer.ts` adapts
 * `@miden-sdk/miden-sdk` (WASM). Tests mock this directly.
 */

import type { HexId, NoteKind } from '@miden-x402/types';

export interface P2idPaymentRequest {
  /** Recipient (merchant) account id. */
  payTo: HexId;
  /** Faucet account id of the asset to send. */
  asset: HexId;
  /** Atomic-unit amount as decimal string. */
  amount: string;
  /**
   * Whether to build a public or private P2ID note. Defaults to `'public'`
   * for backward compatibility with pre-M7 Payer impls. The merchant chooses
   * which note kind to request via `PaymentRequirements.extra.noteType`; the
   * agent forwards that selection here.
   */
  noteType?: NoteKind;
}

export interface P2idPaymentReceipt {
  /** Committed note id (32 bytes / 64 hex chars). */
  noteId: HexId;
  /** Create-note transaction id. */
  transactionId: HexId;
  /** Sender account id (echoed back, normalised). */
  sender: HexId;
  /** Block number in which the note was committed. */
  blockNum: number;
  /**
   * For `noteType: 'private'`, the canonical Miden `NoteFile` serialised and
   * base64-encoded. Omitted for the public path (only the commitment is
   * needed there, and it's already in `noteId`). The agent forwards this
   * into `PrivateP2idPayload.noteBlob`.
   */
  noteBlob?: string;
}

export interface Payer {
  /**
   * Builds + proves + submits a P2ID note, then waits for it to be included
   * in a committed block on Miden testnet. Resolves with the receipt fields
   * the merchant's `Payment-Signature` payload needs.
   */
  payP2ID(request: P2idPaymentRequest): Promise<P2idPaymentReceipt>;

  /**
   * Builds + signs (without proving or submitting) a private P2ID
   * transaction. Returns the canonical serialized inputs the Guardian needs
   * to verify offline and later prove + submit. Optional — only implement
   * if the underlying client/WASM SDK exposes an execute-without-prove
   * path.
   *
   * `serialNum` is server-generated and must be honoured exactly so the
   * Guardian's pre-computed nullifier matches.
   */
  payP2IDUnproven?(request: P2idUnprovenRequest): Promise<P2idUnprovenReceipt>;
}

export interface P2idUnprovenRequest {
  /** Recipient (merchant) account id. */
  payTo: HexId;
  /** Faucet account id of the asset to send. */
  asset: HexId;
  /** Atomic-unit amount as decimal string. */
  amount: string;
  /**
   * Server-issued 32-byte hex `serial_num`. The agent must use this exact
   * value as the P2ID note's serial number; the Guardian pre-computed the
   * resulting nullifier at 402-time and will reject any deviation.
   */
  serialNum: HexId;
}

export interface P2idUnprovenReceipt {
  /** Base64-encoded canonical `TransactionInputs` blob. */
  txInputs: string;
  /**
   * Base64-encoded `miden_protocol::account::auth::Signature` over the
   * `TransactionSummary::to_commitment()` digest. The Guardian uses this
   * for offline verification.
   */
  signature: string;
  /**
   * Base64-encoded `TransactionSummary` — the exact value whose
   * commitment the buyer signed.
   */
  signedSummary: string;
  /**
   * Base64-encoded `NoteFile::NoteDetails` for the new output P2ID note.
   */
  expectedNoteBlob: string;
  /** Pre-prove `TransactionId` derived from `TransactionInputs`. */
  transactionId: HexId;
  /** Sender account id. */
  sender: HexId;
}
