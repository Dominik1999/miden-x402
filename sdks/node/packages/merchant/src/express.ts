/**
 * Express middleware that gates a route behind a Miden x402 payment.
 */

import type { RequestHandler } from 'express';

import {
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
} from '@miden-x402/types';

import {
  encodeRequiredHeader,
  encodeResponseHeader,
  processPayment,
  type PriceTag,
  type PaywallConfig,
} from './core.js';

export interface ExpressPaywallOptions extends PaywallConfig {
  price: PriceTag;
  /**
   * Optional description shown to buyers in the 402 body. Defaults to the
   * request path.
   */
  description?: string;
  /** Optional MIME type advertised in the 402 body. */
  mimeType?: string;
}

/**
 * Returns an Express middleware that:
 *
 *   - emits a `402 Payment Required` with `Payment-Required` header when
 *     `Payment-Signature` is missing or unparseable;
 *   - forwards a present `Payment-Signature` to the facilitator's `/settle`,
 *     attaches the resulting `Payment-Response` header on success, and
 *     proceeds to the gated handler;
 *   - on facilitator failure, re-emits the 402 with `error` populated per
 *     `docs/protocol.md` §A.3.
 */
export function paywall(opts: ExpressPaywallOptions): RequestHandler {
  return async (req, res, next) => {
    const url = `${req.protocol}://${req.get('host') ?? 'localhost'}${req.originalUrl}`;
    const signatureRaw = req.header(PAYMENT_SIGNATURE_HEADER);
    const outcome = await processPayment({
      signatureHeader: signatureRaw,
      price: opts.price,
      resource: {
        url,
        description: opts.description ?? req.originalUrl,
        mimeType: opts.mimeType,
      },
      config: {
        facilitatorUrl: opts.facilitatorUrl,
        fetch: opts.fetch,
        merchantAuth: opts.merchantAuth,
      },
    });

    if (outcome.kind === 'paid') {
      res.setHeader(PAYMENT_RESPONSE_HEADER, encodeResponseHeader(outcome.settle));
      next();
      return;
    }

    res
      .status(402)
      .setHeader('Payment-Required', encodeRequiredHeader(outcome.body))
      .json(outcome.body);
  };
}
