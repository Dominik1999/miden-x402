/**
 * Agent client for x402 v2 on Miden.
 *
 * Wraps a `fetch` implementation so that 402 responses trigger a P2ID
 * note payment (built via the configured `Payer`) and the original
 * request is retried with the `Payment-Signature` header attached.
 *
 * Wire details follow `docs/protocol.md` §A. The `accepted` object echoed
 * in the `Payment-Signature` payload MUST be the exact same object the
 * merchant offered, including unknown future fields.
 */

import {
  ASSET_TRANSFER_METHOD_P2ID,
  EXACT_SCHEME,
  MIDEN_TESTNET,
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_RESPONSE_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  decodePaymentRequiredHeader,
  decodePaymentResponseHeader,
  encodePaymentSignatureHeader,
  type GuardianFastPayload,
  type MidenExactPayload,
  type MidenPaymentPayload,
  type MidenPaymentRequired,
  type MidenPaymentRequirements,
  type PrivateP2idPayload,
  type PublicP2idPayload,
  type SettleResponse,
} from '@miden-x402/types';

import type { Payer } from './payer.js';

export type { Payer, P2idPaymentReceipt, P2idPaymentRequest } from './payer.js';
export {
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
};
export { createWasmSdkPayer, WasmSdkPayerError } from './wasm-sdk-payer.js';
export type { WasmSdkPayerOptions } from './wasm-sdk-payer.js';

export interface AgentOptions {
  /** Implementation that builds + proves + submits the P2ID note. */
  payer: Payer;
  /**
   * Optional callback fired with the merchant's `Payment-Response` settle
   * body once the request succeeds. Useful for logging transaction ids.
   */
  onSettlement?: (settle: Extract<SettleResponse, { success: true }>) => void;
  /**
   * Optional callback fired right before retrying with the signed payload.
   * Lets callers inspect or store the receipt.
   */
  onPaymentBuilt?: (payload: MidenPaymentPayload) => void;
}

/**
 * Wraps a `fetch` so that 402 responses are paid for and the original
 * request is automatically retried with `Payment-Signature` attached.
 *
 * Note: the wrapped fetch retries the request **once**. If the merchant
 * still returns 402 after a successful payment (e.g. the facilitator
 * rejected the note as already-consumed), the 402 is propagated to the
 * caller with the merchant's error body.
 */
export function withMidenX402(baseFetch: typeof fetch, opts: AgentOptions): typeof fetch {
  return async function midenX402Fetch(input, init) {
    const response = await baseFetch(input, init);
    if (response.status !== 402) {
      return response;
    }

    const requiredHeader = response.headers.get(PAYMENT_REQUIRED_HEADER);
    if (!requiredHeader) {
      return response;
    }

    const required = decodePaymentRequiredHeader(requiredHeader);
    const requirements = pickMidenAccept(required);
    if (!requirements) {
      return response;
    }

    const settlement = requirements.extra.settlement ?? 'commit';

    let inner: MidenExactPayload;
    if (settlement === 'guardian-fast') {
      if (typeof opts.payer.payP2IDUnproven !== 'function') {
        throw new Error(
          'agent: requirements demand settlement="guardian-fast" but the configured Payer does not implement payP2IDUnproven',
        );
      }
      if (!requirements.extra.serialNum) {
        throw new Error(
          'agent: guardian-fast requirements are missing extra.serialNum (the server-generated challenge)',
        );
      }
      const receipt = await opts.payer.payP2IDUnproven({
        payTo: requirements.payTo,
        asset: requirements.asset,
        amount: requirements.amount,
        serialNum: requirements.extra.serialNum,
      });
      inner = {
        noteType: 'guardianFast',
        txInputs: receipt.txInputs,
        signature: receipt.signature,
        signedSummary: receipt.signedSummary,
        expectedNoteBlob: receipt.expectedNoteBlob,
        serialNum: requirements.extra.serialNum,
        transactionId: receipt.transactionId,
        sender: receipt.sender,
        asset: requirements.asset,
        amount: requirements.amount,
      } satisfies GuardianFastPayload;
    } else {
      const receipt = await opts.payer.payP2ID({
        payTo: requirements.payTo,
        asset: requirements.asset,
        amount: requirements.amount,
        noteType: requirements.extra.noteType,
      });
      if (requirements.extra.noteType === 'private') {
        if (!receipt.noteBlob) {
          throw new Error(
            'agent: payer returned no noteBlob for a private-note request; the Payer impl must export the canonical NoteFile for private payments',
          );
        }
        inner = {
          noteType: 'private',
          noteBlob: receipt.noteBlob,
          transactionId: receipt.transactionId,
          sender: receipt.sender,
          blockNum: receipt.blockNum,
          asset: requirements.asset,
          amount: requirements.amount,
        } satisfies PrivateP2idPayload;
      } else {
        inner = {
          noteType: 'public',
          noteId: receipt.noteId,
          transactionId: receipt.transactionId,
          sender: receipt.sender,
          blockNum: receipt.blockNum,
          asset: requirements.asset,
          amount: requirements.amount,
        } satisfies PublicP2idPayload;
      }
    }

    const payload: MidenPaymentPayload = {
      x402Version: 2,
      accepted: requirements,
      payload: inner,
    };

    opts.onPaymentBuilt?.(payload);

    const signedInit = withSignatureHeader(init, encodePaymentSignatureHeader(payload));
    const retried = await baseFetch(input, signedInit);

    if (retried.ok) {
      const settleHeader = retried.headers.get(PAYMENT_RESPONSE_HEADER);
      if (settleHeader && opts.onSettlement) {
        try {
          const settle = decodePaymentResponseHeader(settleHeader);
          if (settle.success) opts.onSettlement(settle);
        } catch {
          // Ignore: merchants are allowed to omit the header.
        }
      }
    }
    return retried;
  } as typeof fetch;
}

function pickMidenAccept(req: MidenPaymentRequired): MidenPaymentRequirements | null {
  // Both public and private noteType variants are supported; the agent
  // forwards the merchant's choice to the Payer.
  for (const accept of req.accepts) {
    if (
      accept.scheme === EXACT_SCHEME &&
      accept.network === MIDEN_TESTNET &&
      accept.extra.assetTransferMethod === ASSET_TRANSFER_METHOD_P2ID &&
      (accept.extra.noteType === 'public' || accept.extra.noteType === 'private')
    ) {
      return accept;
    }
  }
  return null;
}

function withSignatureHeader(init: RequestInit | undefined, value: string): RequestInit {
  const headers = new Headers(init?.headers ?? {});
  headers.set(PAYMENT_SIGNATURE_HEADER, value);
  return { ...init, headers };
}
