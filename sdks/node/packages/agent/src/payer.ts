/**
 * Agent-side `Payer` interface for the Guardian-facilitator wire.
 *
 * Builds a signed-but-unproven Miden transaction whose output P2ID note
 * matches the merchant's `MidenPaymentRequirements`, packages it into a
 * `MidenP2idPrivatePayload`, and (in the full E2E flow) submits it to the
 * Guardian-facilitator's `/x402/settle` endpoint.
 *
 * The reference implementation is intentionally a stub — building a signed
 * unproven `TransactionInputs` from JS requires WASM SDK extensions in
 * `@miden-sdk/miden-sdk` that don't exist yet (the SDK can prove + submit,
 * but the "build + sign + STOP before proving" seam isn't exposed). Rust
 * agents work end-to-end today via the `miden-multisig-client` crate in the
 * OZ Guardian repo; Node + browser agents need this upstream work.
 *
 * Tracked in `docs/UPSTREAM_WISHLIST.md`.
 */

import type { HexId, MidenP2idPrivatePayload } from '@miden-x402/types';

export interface UnprovenTxRequest {
  /** Buyer (Miden) account id. */
  buyerAccountId: HexId;
  /** Merchant account id (`requirements.payTo`). */
  payTo: HexId;
  /** Faucet account id (`requirements.asset`). */
  asset: HexId;
  /** Atomic-unit amount as decimal string. */
  amount: string;
  /** Server-issued `serial_num` (`requirements.extra.serialNum`). */
  serialNum: HexId;
  /** Note tag (`requirements.extra.noteTag`). */
  noteTag: string;
}

/**
 * A `Payer` builds and signs an unproven Miden transaction matching the
 * merchant's requirements. The Guardian-facilitator does the actual prove
 * + submit; the agent's only on-chain work is signing.
 */
export interface Payer {
  buildUnprovenPayment(req: UnprovenTxRequest): Promise<MidenP2idPrivatePayload>;
}

/** Thrown when no concrete `Payer` implementation is wired up. */
export class PayerNotImplemented extends Error {
  constructor(reason: string) {
    super(`agent payer not yet implemented: ${reason}`);
    this.name = 'PayerNotImplemented';
  }
}

/**
 * Reference `Payer` stub. Throws on use until WASM SDK extensions ship.
 * Provided so callers can wire the agent flow end-to-end and surface a
 * useful error instead of `undefined`.
 */
export class StubPayer implements Payer {
  async buildUnprovenPayment(_: UnprovenTxRequest): Promise<MidenP2idPrivatePayload> {
    throw new PayerNotImplemented(
      'building a signed unproven TransactionInputs from JS requires ' +
        '`@miden-sdk/miden-sdk` extensions that are tracked upstream — see ' +
        'docs/UPSTREAM_WISHLIST.md. Use the `miden-multisig-client` Rust crate ' +
        'from the OZ Guardian repo for E2E agents today.',
    );
  }
}
