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
  type SettlementKind,
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
  /** Note privacy. */
  noteType?: NoteKind;
  /**
   * Settlement model. Defaults to `'commit'` (Phase A, settled-at-commit).
   * Set to `'guardian-fast'` to opt into the verify-before-prove flow —
   * requires `config.facilitatorUrl` to point at a Guardian-enabled
   * facilitator (with `MIDEN_X402_GUARDIAN_ENABLED=true`), and Phase B
   * mandates `noteType: 'private'`.
   */
  settlement?: SettlementKind;
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

/**
 * Builds the requirements without acquiring a Guardian challenge. The
 * `serialNum`/`guardianUrl` fields are left absent — the caller (typically
 * `processPayment`) is expected to populate them via
 * {@link acquireGuardianChallenge} when `settlement: 'guardian-fast'`.
 */
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
      ...(price.settlement && price.settlement !== 'commit'
        ? { settlement: price.settlement }
        : {}),
    },
  };
}

/**
 * For `settlement: 'guardian-fast'`, asks the facilitator to issue a
 * server-generated `serial_num` for this payment offer. Returns the
 * requirements with `serialNum` and `guardianUrl` populated.
 */
export async function acquireGuardianChallenge(
  requirements: MidenPaymentRequirements,
  config: PaywallConfig,
): Promise<MidenPaymentRequirements> {
  const fetchImpl = config.fetch ?? fetch;
  const base = config.facilitatorUrl.replace(/\/$/, '');
  const response = await fetchImpl(`${base}/guardian/challenge`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ paymentRequirements: requirements }),
  });
  if (!response.ok) {
    throw new Error(
      `facilitator /guardian/challenge returned ${response.status}: ${await response.text()}`,
    );
  }
  const body = (await response.json()) as {
    serialNum: HexId;
    expiresInSeconds: number;
  };
  return {
    ...requirements,
    extra: {
      ...requirements.extra,
      serialNum: body.serialNum,
      guardianUrl: base,
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
  // Route to the right endpoint based on the negotiated settlement model.
  const settlement: SettlementKind =
    requirements.extra.settlement ?? 'commit';
  const path = settlement === 'guardian-fast' ? '/guardian/settle' : '/settle';
  let response: Response;
  try {
    response = await fetchImpl(`${config.facilitatorUrl.replace(/\/$/, '')}${path}`, {
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
 *
 * For `price.settlement: 'guardian-fast'`, on the first request (no payment
 * header) this also acquires a server-generated `serial_num` from the
 * facilitator's `/guardian/challenge` endpoint and embeds it in the 402's
 * `extra.serialNum`. On the retry it forwards the signed unproven payload
 * to `/guardian/settle`.
 */
export async function processPayment(args: {
  signatureHeader: string | undefined;
  price: PriceTag;
  resource: ResourceInfo;
  config: PaywallConfig;
}): Promise<PaymentOutcome> {
  const settlement = args.price.settlement ?? 'commit';
  const payload = tryDecodeSignature(args.signatureHeader);
  if (!payload) {
    // First hit — emit a 402. For guardian-fast we go acquire a challenge.
    let requirements = buildRequirements(args.price);
    if (settlement === 'guardian-fast') {
      try {
        requirements = await acquireGuardianChallenge(requirements, args.config);
      } catch (e) {
        // If the facilitator's challenge endpoint is unavailable, surface
        // the error in the 402 body so the buyer sees something actionable.
        return {
          kind: '402',
          body: {
            x402Version: 2,
            accepts: [requirements],
            resource: args.resource,
            error: `failed to acquire guardian challenge: ${(e as Error).message}`,
          },
        };
      }
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

  // Retry with a Payment-Signature header. Use the requirements echoed in
  // `payload.accepted` (which carries the guardian challenge serialNum that
  // we issued in the 402) rather than re-deriving from the price.
  const requirements = payload.accepted;
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
