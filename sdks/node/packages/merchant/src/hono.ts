/**
 * Hono middleware that gates a route behind a Miden x402 payment.
 */

import type { Context, MiddlewareHandler } from 'hono';

import {
  PAYMENT_REQUIRED_HEADER,
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

export interface HonoPaywallOptions extends PaywallConfig {
  price: PriceTag;
  description?: string;
  mimeType?: string;
}

export function paywall(opts: HonoPaywallOptions): MiddlewareHandler {
  return async (c: Context, next) => {
    const url = new URL(c.req.url).toString();
    const signatureRaw = c.req.header(PAYMENT_SIGNATURE_HEADER) ?? undefined;
    const outcome = await processPayment({
      signatureHeader: signatureRaw,
      price: opts.price,
      resource: {
        url,
        description: opts.description ?? new URL(c.req.url).pathname,
        mimeType: opts.mimeType,
      },
      config: {
        facilitatorUrl: opts.facilitatorUrl,
        fetch: opts.fetch,
        merchantAuth: opts.merchantAuth,
      },
    });

    if (outcome.kind === 'paid') {
      c.header(PAYMENT_RESPONSE_HEADER, encodeResponseHeader(outcome.settle));
      await next();
      return;
    }

    c.header(PAYMENT_REQUIRED_HEADER, encodeRequiredHeader(outcome.body));
    return c.json(outcome.body, 402);
  };
}
