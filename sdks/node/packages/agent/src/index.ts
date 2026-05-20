/**
 * Agent client for the `miden-p2id-private` scheme on Miden via the
 * Guardian-facilitator.
 *
 * Wraps a `fetch` so a 402 response triggers an unproven Miden tx build
 * (via the configured {@link Payer}), and the original request is retried
 * with the `Payment-Signature` header attached.
 *
 * Wire details follow `docs/protocol.md`. The `accepted` object echoed in
 * the `Payment-Signature` payload MUST be the exact same object the
 * merchant offered, including unknown future fields.
 */

import {
  MIDEN_P2ID_PRIVATE_SCHEME,
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_RESPONSE_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  decodePaymentRequiredHeader,
  decodePaymentResponseHeader,
  encodePaymentSignatureHeader,
  type MidenPaymentPayload,
  type MidenPaymentRequired,
  type MidenPaymentRequirements,
  type SettleSuccess,
} from '@miden-x402/types';

import type { Payer } from './payer.js';

export { StubPayer, PayerNotImplemented } from './payer.js';
export type { Payer, UnprovenTxRequest } from './payer.js';
export {
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
};

export interface AgentOptions {
  /** The buyer's Miden account id (canonical hex). */
  buyerAccountId: string;
  /** Implementation that builds + signs the unproven Miden transaction. */
  payer: Payer;
  /**
   * Optional callback fired with the facilitator's settle receipt once
   * delivery succeeds. Useful for logging or persisting `queuedId`.
   */
  onSettlement?: (settle: SettleSuccess) => void;
  /**
   * Optional callback fired right before retrying with the signed payload.
   * Lets callers inspect or store the receipt.
   */
  onPaymentBuilt?: (payload: MidenPaymentPayload) => void;
}

/**
 * Wraps a `fetch` so 402 responses are paid for and the original request
 * is automatically retried with `Payment-Signature` attached.
 *
 * The wrapped fetch retries the request **once**. If the merchant returns
 * 402 again after a successful payment, the 402 is propagated to the
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
    const serialNum = requirements.extra.serialNum;
    if (!serialNum) {
      throw new Error(
        'agent: 402 is missing extra.serialNum — merchant must call POST /x402/challenge ' +
          'before emitting the 402 (see docs/protocol.md)',
      );
    }

    const inner = await opts.payer.buildUnprovenPayment({
      buyerAccountId: opts.buyerAccountId,
      payTo: requirements.payTo,
      asset: requirements.asset,
      amount: requirements.amount,
      serialNum,
      noteTag: requirements.extra.noteTag,
    });

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
          // Header is optional / merchants may omit.
        }
      }
    }
    return retried;
  } as typeof fetch;
}

function pickMidenAccept(req: MidenPaymentRequired): MidenPaymentRequirements | null {
  for (const accept of req.accepts) {
    if (accept.scheme === MIDEN_P2ID_PRIVATE_SCHEME) return accept;
  }
  return null;
}

function withSignatureHeader(init: RequestInit | undefined, value: string): RequestInit {
  const headers = new Headers(init?.headers ?? {});
  headers.set(PAYMENT_SIGNATURE_HEADER, value);
  return { ...init, headers };
}
