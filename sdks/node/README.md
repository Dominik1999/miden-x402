# miden-x402 — Node.js SDK

Reference TypeScript packages for the `miden-p2id-private` x402 scheme.
Wire format matches [`docs/protocol.md`](../../docs/protocol.md) and the
Rust crates in [`crates/`](../../crates/).

## Packages

| Package | Purpose | Status |
|---|---|---|
| [`@miden-x402/types`](packages/types) | Wire types + base64 header codecs | shipped |
| [`@miden-x402/merchant`](packages/merchant) | Express + Hono paywall middleware | shipped |
| [`@miden-x402/agent`](packages/agent) | Drop-in `fetch` wrapper that pays a 402 | stub (see "Agent" below) |

## Merchant

```ts
import express from 'express';
import { paywall } from '@miden-x402/merchant/express';

const app = express();
app.get(
  '/weather',
  paywall({
    facilitatorUrl: 'https://facilitator.miden.example',
    merchantAuth: myGuardianAuth,
    price: {
      amount: '1000',
      asset: '0x0a7d175ed63ec5200fb2ced86f6aa5',
      payTo: '0x...your-merchant-account-id...',
      noteTag: 'weather.api',
    },
  }),
  (_req, res) => res.json({ temperature: 21.5 }),
);
```

`merchantAuth` is a `MerchantAuth` impl (see
[`packages/merchant/src/core.ts`](packages/merchant/src/core.ts)) that
signs the Guardian-style `x-pubkey`/`x-signature`/`x-timestamp` headers
for each call to `POST /x402/challenge` and `POST /x402/settle`. The
merchant is a Guardian-registered account; the operator provisions a
Falcon cosigner key for it and wires up the signer.

The middleware:

1. On the first request, calls `POST /x402/challenge` to obtain a
   `serial_num` and emits a `402 Payment Required` with it.
2. On the retry (with `Payment-Signature` attached), forwards the
   payload to `POST /x402/settle` and emits the Guardian-signed
   `Payment-Response` header on success.

For Hono, use `paywall` from `@miden-x402/merchant/hono`. The API is
identical.

## Agent

`@miden-x402/agent` exports a `withMidenX402(fetch, opts)` wrapper. The
reference `Payer` is a stub that throws — building a signed unproven
Miden transaction from JS requires `@miden-sdk/miden-sdk` extensions
that don't exist yet. Track upstream in
[`docs/UPSTREAM_WISHLIST.md`](../../docs/UPSTREAM_WISHLIST.md).

For Rust agents, use [`miden-multisig-client`](https://github.com/OpenZeppelin/guardian/tree/main/crates/miden-multisig-client)
from the OZ Guardian repo today.

## Building

This is a pnpm workspace:

```bash
cd sdks/node
pnpm install
pnpm -r build
```

## Where to look next

- Wire contract: [`docs/protocol.md`](../../docs/protocol.md)
- Design write-up: [`ideas/DESIGN.md`](../../ideas/DESIGN.md)
- Deployment / env vars: [`docs/deploy.md`](../../docs/deploy.md)
- Mandate enforcement: [`docs/mandate.md`](../../docs/mandate.md)
- Upstream PR wishlist: [`docs/UPSTREAM_WISHLIST.md`](../../docs/UPSTREAM_WISHLIST.md)
