/**
 * Framework-agnostic merchant logic.
 *
 * The merchant never verifies the payment itself; it just forwards the
 * decoded payload + the matched requirements to the configured facilitator
 * and acts on the response. See `docs/protocol.md` §A.2 for the wire
 * exchange this implements.
 */

import {
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
  EXACT_SCHEME,
  ASSET_TRANSFER_METHOD_P2ID,
  MIDEN_TESTNET,
  decodePaymentSignatureHeader,
  encodePaymentRequiredHeader,
  encodePaymentResponseHeader,
  type MidenPaymentRequired,
  type MidenPaymentRequirements,
  type MidenPaymentPayload,
  type ResourceInfo,
  type SettleResponse,
  type VerifyResponse,
  type HexId,
  type DecimalAmount,
  type MidenNetwork,
  type NoteKind,
} from '@miden-x402/types';

export {
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
};

/**
 * What the merchant wants paid for the gated resource. One `PriceTag` maps
 * to one entry in the 402's `accepts[]` array. Future versions may support
 * multiple accepts (e.g. testnet + mainnet); for now we ship a single entry.
 */
export interface PriceTag {
  /** Atomic-unit amount (e.g. `"1000"` = 1000 micro-USDC). */
  amount: DecimalAmount;
  /** Faucet account id of the accepted asset. */
  asset: HexId;
  /** Merchant account id that will receive the P2ID note. */
  payTo: HexId;
  /** Network identifier. Defaults to `miden:testnet`. */
  network?: MidenNetwork;
  /** Display token symbol for the buyer's UX (informational). */
  tokenSymbol: string;
  /** Token decimals (informational). */
  decimals: number;
  /** Note privacy. Only `"public"` is supported in MVP. */
  noteType?: NoteKind;
  /** Seconds the merchant is willing to wait for the note to commit. */
  maxTimeoutSeconds?: number;
}

export interface PaywallConfig {
  /** Base URL of the Miden x402 facilitator. */
  facilitatorUrl: string;
  /** Optional fetch implementation override (for testing). */
  fetch?: typeof fetch;
}

export interface VerifyResult {
  ok: boolean;
  /** When `ok`, the underlying facilitator response on success. */
  settle?: Extract<SettleResponse, { success: true }>;
  /** When `!ok`, the merchant-facing error string for the next 402 body. */
  error?: string;
}

export function buildRequirements(price: PriceTag): MidenPaymentRequirements {
  return {
    scheme: EXACT_SCHEME,
    network: price.network ?? MIDEN_TESTNET,
    amount: price.amount,
    asset: price.asset,
    payTo: price.payTo,
    maxTimeoutSeconds: price.maxTimeoutSeconds ?? 120,
    extra: {
      assetTransferMethod: ASSET_TRANSFER_METHOD_P2ID,
      tokenSymbol: price.tokenSymbol,
      decimals: price.decimals,
      noteType: price.noteType ?? 'public',
    },
  };
}

export function buildPaymentRequired(
  price: PriceTag,
  resource: ResourceInfo,
  error?: string,
): MidenPaymentRequired {
  const body: MidenPaymentRequired = {
    x402Version: 2,
    accepts: [buildRequirements(price)],
    resource,
  };
  if (error) body.error = error;
  return body;
}

/**
 * Decodes a `Payment-Signature` header value into the canonical payload.
 * Returns `null` if the header is missing or unparseable. Callers should
 * treat `null` as "buyer has not paid yet; serve a 402".
 */
export function tryDecodeSignature(headerValue: string | undefined): MidenPaymentPayload | null {
  if (!headerValue) return null;
  try {
    return decodePaymentSignatureHeader(headerValue);
  } catch {
    return null;
  }
}

export function encodeRequiredHeader(body: MidenPaymentRequired): string {
  return encodePaymentRequiredHeader(body);
}

export function encodeResponseHeader(body: SettleResponse): string {
  return encodePaymentResponseHeader(body);
}

/**
 * Calls the facilitator's `/settle`. Returns either the success body or an
 * error message suitable for the second 402's `error` field.
 *
 * We only call `/settle` (not `/verify` first) because under the Miden
 * settled-at-commit model `/settle` re-runs the same checks as `/verify`
 * and is idempotent. Skipping the extra hop halves the round-trip latency.
 */
export async function settleWithFacilitator(
  payload: MidenPaymentPayload,
  requirements: MidenPaymentRequirements,
  config: PaywallConfig,
): Promise<VerifyResult> {
  const fetchImpl = config.fetch ?? fetch;
  let response: Response;
  try {
    response = await fetchImpl(`${config.facilitatorUrl.replace(/\/$/, '')}/settle`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        x402Version: 2,
        paymentPayload: payload,
        paymentRequirements: requirements,
      }),
    });
  } catch (e) {
    return { ok: false, error: `facilitator unreachable: ${(e as Error).message}` };
  }

  if (!response.ok) {
    const text = await response.text();
    let reason = `facilitator returned ${response.status}`;
    try {
      const body = JSON.parse(text) as {
        invalidReason?: string;
        invalidReasonDetails?: string;
      };
      reason = body.invalidReasonDetails || body.invalidReason || reason;
    } catch {
      reason = text || reason;
    }
    return { ok: false, error: reason };
  }

  const body = (await response.json()) as SettleResponse;
  if (body.success) {
    return { ok: true, settle: body };
  }
  return { ok: false, error: body.errorReason };
}

export type PaymentOutcome =
  | { kind: 'paid'; settle: Extract<SettleResponse, { success: true }> }
  | { kind: '402'; body: MidenPaymentRequired };

/**
 * High-level handler: given the incoming request's `Payment-Signature` header,
 * the resource info, and the price, returns a structured outcome the
 * framework adapter can translate into an HTTP response.
 */
export async function processPayment(args: {
  signatureHeader: string | undefined;
  price: PriceTag;
  resource: ResourceInfo;
  config: PaywallConfig;
}): Promise<PaymentOutcome> {
  const payload = tryDecodeSignature(args.signatureHeader);
  if (!payload) {
    return {
      kind: '402',
      body: buildPaymentRequired(args.price, args.resource),
    };
  }
  const requirements = buildRequirements(args.price);
  const result = await settleWithFacilitator(payload, requirements, args.config);
  if (result.ok && result.settle) {
    return { kind: 'paid', settle: result.settle };
  }
  return {
    kind: '402',
    body: buildPaymentRequired(args.price, args.resource, result.error ?? 'verification failed'),
  };
}

/** Re-exported for advanced callers that want to verify without settling. */
export async function verifyWithFacilitator(
  payload: MidenPaymentPayload,
  requirements: MidenPaymentRequirements,
  config: PaywallConfig,
): Promise<VerifyResponse> {
  const fetchImpl = config.fetch ?? fetch;
  const response = await fetchImpl(`${config.facilitatorUrl.replace(/\/$/, '')}/verify`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      x402Version: 2,
      paymentPayload: payload,
      paymentRequirements: requirements,
    }),
  });
  return (await response.json()) as VerifyResponse;
}
