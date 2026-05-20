/**
 * Framework-agnostic merchant logic for the Guardian-facilitator wire.
 *
 * The merchant never verifies the payment itself; it forwards the decoded
 * payload + the matched requirements to the configured Guardian-facilitator
 * and acts on the response. See `docs/protocol.md` of the parent repo for
 * the wire exchange.
 */

import {
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
  MIDEN_P2ID_PRIVATE_SCHEME,
  MIDEN_TESTNET,
  decodePaymentSignatureHeader,
  encodePaymentRequiredHeader,
  encodePaymentResponseHeader,
  type ChallengeResponse,
  type DecimalAmount,
  type HexId,
  type MidenNetwork,
  type MidenPaymentPayload,
  type MidenPaymentRequired,
  type MidenPaymentRequirements,
  type ResourceInfo,
  type SettleResponse,
  type SettleSuccess,
  type VerifyResponse,
} from '@miden-x402/types';

export {
  PAYMENT_REQUIRED_HEADER,
  PAYMENT_SIGNATURE_HEADER,
  PAYMENT_RESPONSE_HEADER,
};

/**
 * What the merchant wants paid for the gated resource. One `PriceTag` maps
 * to one entry in the 402's `accepts[]` array.
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
  /** Note-tag the merchant attaches for routing incoming notes. */
  noteTag: string;
  /** Seconds the merchant is willing to wait for settlement. */
  maxTimeoutSeconds?: number;
}

export interface PaywallConfig {
  /** Base URL of the Guardian-facilitator. */
  facilitatorUrl: string;
  /**
   * Optional Guardian-style auth credentials for the merchant. Required by
   * `POST /x402/challenge`, which is a Guardian-authenticated endpoint. If
   * absent the merchant SDK skips the challenge step and emits a 402
   * without a `serialNum` — the buyer's SDK must then call `/x402/challenge`
   * itself (acceptable but less efficient).
   */
  merchantAuth?: MerchantAuth;
  /** Optional fetch implementation override (for testing). */
  fetch?: typeof fetch;
}

/**
 * Guardian-style auth credentials for the merchant account. Producing valid
 * signatures requires Falcon-512 over `RPO256([account_id, ts, payload])` —
 * see Guardian's `spec/api.md` § Miden Request Signing. The SDK takes a
 * pluggable signer rather than embedding the crypto so merchants can choose
 * their own key-management story.
 */
export interface MerchantAuth {
  /** The merchant's Miden account id (canonical hex). */
  accountId: HexId;
  /**
   * Returns the `x-pubkey`, `x-signature`, `x-timestamp` headers for a
   * given request body. The signer is responsible for computing the
   * payload digest and signing it.
   */
  signRequest(body: string): Promise<{
    'x-pubkey': string;
    'x-signature': string;
    'x-timestamp': string;
  }>;
}

export interface VerifyResult {
  ok: boolean;
  settle?: SettleSuccess;
  error?: string;
}

/**
 * Builds the requirements without acquiring a `serial_num`. The caller is
 * expected to call {@link acquireChallenge} to populate `extra.serialNum`
 * before emitting the 402.
 */
export function buildRequirements(price: PriceTag): MidenPaymentRequirements {
  return {
    scheme: MIDEN_P2ID_PRIVATE_SCHEME,
    network: price.network ?? MIDEN_TESTNET,
    amount: price.amount,
    asset: price.asset,
    payTo: price.payTo,
    maxTimeoutSeconds: price.maxTimeoutSeconds ?? 120,
    extra: { noteTag: price.noteTag },
  };
}

/**
 * Asks the facilitator for a server-generated `serial_num`. Returns the
 * requirements with `extra.serialNum` populated. Authenticates the call
 * with `config.merchantAuth` when supplied.
 */
export async function acquireChallenge(
  requirements: MidenPaymentRequirements,
  config: PaywallConfig,
): Promise<MidenPaymentRequirements> {
  const fetchImpl = config.fetch ?? fetch;
  const base = config.facilitatorUrl.replace(/\/$/, '');
  const body = JSON.stringify({ paymentRequirements: requirements });
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (config.merchantAuth) {
    Object.assign(headers, await config.merchantAuth.signRequest(body));
  }
  const response = await fetchImpl(`${base}/x402/challenge`, {
    method: 'POST',
    headers,
    body,
  });
  if (!response.ok) {
    throw new Error(
      `facilitator /x402/challenge returned ${response.status}: ${await response.text()}`,
    );
  }
  const json = (await response.json()) as ChallengeResponse;
  return {
    ...requirements,
    extra: { ...requirements.extra, serialNum: json.serialNum },
  };
}

export function buildPaymentRequired(
  price: PriceTag,
  resource: ResourceInfo,
  error?: string,
  serialNum?: HexId,
): MidenPaymentRequired {
  const requirements = buildRequirements(price);
  if (serialNum) requirements.extra.serialNum = serialNum;
  const body: MidenPaymentRequired = {
    x402Version: 2,
    accepts: [requirements],
    resource,
  };
  if (error) body.error = error;
  return body;
}

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
 * Calls the Guardian-facilitator's `/x402/settle`. Returns either the
 * success body (with `receiptSig` for the merchant to retain) or an error
 * message suitable for a follow-up 402's `error` field.
 */
export async function settleWithFacilitator(
  payload: MidenPaymentPayload,
  requirements: MidenPaymentRequirements,
  config: PaywallConfig,
): Promise<VerifyResult> {
  const fetchImpl = config.fetch ?? fetch;
  const base = config.facilitatorUrl.replace(/\/$/, '');
  const body = JSON.stringify({
    x402Version: 2,
    paymentPayload: payload,
    paymentRequirements: requirements,
  });
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (config.merchantAuth) {
    Object.assign(headers, await config.merchantAuth.signRequest(body));
  }

  let response: Response;
  try {
    response = await fetchImpl(`${base}/x402/settle`, { method: 'POST', headers, body });
  } catch (e) {
    return { ok: false, error: `facilitator unreachable: ${(e as Error).message}` };
  }

  if (!response.ok) {
    const text = await response.text();
    let reason = `facilitator returned ${response.status}`;
    try {
      const errBody = JSON.parse(text) as {
        invalidReason?: string;
        invalidReasonDetails?: string;
      };
      reason = errBody.invalidReasonDetails || errBody.invalidReason || reason;
    } catch {
      reason = text || reason;
    }
    return { ok: false, error: reason };
  }

  const settleBody = (await response.json()) as SettleResponse;
  if (settleBody.success) {
    return { ok: true, settle: settleBody };
  }
  return { ok: false, error: settleBody.errorReason };
}

export type PaymentOutcome =
  | { kind: 'paid'; settle: SettleSuccess }
  | { kind: '402'; body: MidenPaymentRequired };

/**
 * High-level handler: given the incoming request's `Payment-Signature` header,
 * the resource info, and the price, returns a structured outcome the
 * framework adapter can translate into an HTTP response.
 *
 * On the first request (no payment header) acquires a `serial_num` from the
 * facilitator's `POST /x402/challenge` endpoint and embeds it in the 402's
 * `extra.serialNum`. On the retry it forwards the signed unproven payload to
 * `POST /x402/settle`.
 */
export async function processPayment(args: {
  signatureHeader: string | undefined;
  price: PriceTag;
  resource: ResourceInfo;
  config: PaywallConfig;
}): Promise<PaymentOutcome> {
  const payload = tryDecodeSignature(args.signatureHeader);
  if (!payload) {
    let requirements = buildRequirements(args.price);
    try {
      requirements = await acquireChallenge(requirements, args.config);
    } catch (e) {
      return {
        kind: '402',
        body: {
          x402Version: 2,
          accepts: [requirements],
          resource: args.resource,
          error: `failed to acquire challenge: ${(e as Error).message}`,
        },
      };
    }
    return {
      kind: '402',
      body: {
        x402Version: 2,
        accepts: [requirements],
        resource: args.resource,
      },
    };
  }

  const requirements = payload.accepted;
  const result = await settleWithFacilitator(payload, requirements, args.config);
  if (result.ok && result.settle) {
    return { kind: 'paid', settle: result.settle };
  }
  return {
    kind: '402',
    body: buildPaymentRequired(
      args.price,
      args.resource,
      result.error ?? 'verification failed',
    ),
  };
}

/** For advanced callers that want to verify without enqueueing. */
export async function verifyWithFacilitator(
  payload: MidenPaymentPayload,
  requirements: MidenPaymentRequirements,
  config: PaywallConfig,
): Promise<VerifyResponse> {
  const fetchImpl = config.fetch ?? fetch;
  const base = config.facilitatorUrl.replace(/\/$/, '');
  const body = JSON.stringify({
    x402Version: 2,
    paymentPayload: payload,
    paymentRequirements: requirements,
  });
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (config.merchantAuth) {
    Object.assign(headers, await config.merchantAuth.signRequest(body));
  }
  const response = await fetchImpl(`${base}/x402/verify`, {
    method: 'POST',
    headers,
    body,
  });
  return (await response.json()) as VerifyResponse;
}
