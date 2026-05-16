/**
 * Abstract `Payer` interface: anything that can build, prove, and submit a
 * public P2ID note on Miden testnet, and wait for it to land in a committed
 * block.
 *
 * The reference implementation in `wasm-sdk-payer.ts` adapts
 * `@miden-sdk/miden-sdk` (WASM). Tests mock this directly.
 */

import type { HexId } from '@miden-x402/types';

export interface P2idPaymentRequest {
  /** Recipient (merchant) account id. */
  payTo: HexId;
  /** Faucet account id of the asset to send. */
  asset: HexId;
  /** Atomic-unit amount as decimal string. */
  amount: string;
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
}

export interface Payer {
  /**
   * Builds + proves + submits a public P2ID note, then waits for it to be
   * included in a committed block on Miden testnet. Resolves with the
   * receipt fields the merchant's `Payment-Signature` payload needs.
   */
  payP2ID(request: P2idPaymentRequest): Promise<P2idPaymentReceipt>;
}
