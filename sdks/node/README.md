# miden-x402 — Node.js SDK

Reference TypeScript packages for x402 v2 on Miden, plus three runnable demos.
Wire format matches [`docs/protocol.md`](../../docs/protocol.md) and the Rust
crates in [`crates/`](../../crates/).

## Packages

- [`@miden-x402/types`](./packages/types) — wire-format types + base64 header
  codecs. Pure TS, no runtime deps.
- [`@miden-x402/merchant`](./packages/merchant) — framework-agnostic core +
  Express adapter (`@miden-x402/merchant/express`) + Hono adapter
  (`@miden-x402/merchant/hono`).
- [`@miden-x402/agent`](./packages/agent) — fetch wrapper that pays a 402 by
  building, proving, and submitting a P2ID note. Default `Payer` adapts the
  `@miden-sdk/miden-sdk` WASM SDK; bring your own `Payer` to plug in any
  other Miden client.

## Demos

| Demo | Path | What it does |
|---|---|---|
| `demo-merchant-express` | [`examples/demo-merchant-express`](./examples/demo-merchant-express) | Express server with `GET /weather` (public note) and `GET /weather-private` (private note), both behind 1000 atomic-unit USDC |
| `demo-merchant-hono`    | [`examples/demo-merchant-hono`](./examples/demo-merchant-hono)    | Hono parity of the above, same two routes |
| `demo-agent`            | [`examples/demo-agent`](./examples/demo-agent)            | CLI that pays the demo merchant; supports both public and private notes, with a mock-payer mode for wiring tests |

## Quickstart

```
# 1. install + build
pnpm install
pnpm build

# 2. run the facilitator (from the repo root)
cargo run -p miden-x402-facilitator --bin miden-x402-facilitator

# 3. run a merchant
MERCHANT_PAY_TO=0x... PORT=3001 pnpm --filter demo-merchant-express start
# or:
MERCHANT_PAY_TO=0x... PORT=3001 pnpm --filter demo-merchant-hono start

# 4. drive a payment (real WASM SDK)
AGENT_BUYER=0x... AGENT_STORE_PATH=./buyer-store \
  TARGET_URL=http://localhost:3001/weather \
  pnpm --filter demo-agent start
```

## Mock-mode demo (no Miden network required)

The demo agent supports a `AGENT_MOCK=1` mode where the `Payer` returns
caller-supplied note identifiers. The facilitator still calls the live
Miden node — for a fake note id it returns `note not committed: …`, which
the merchant surfaces as the `error` field of a second 402. This validates
the full HTTP wiring without needing a funded buyer:

```
# in shell A
cargo run -p miden-x402-facilitator --bin miden-x402-facilitator

# in shell B
MERCHANT_PAY_TO=0x103f8a1ad4b983104aec0412ab0b0d PORT=3001 \
  pnpm --filter demo-merchant-express start

# in shell C
AGENT_MOCK=1 TARGET_URL=http://localhost:3001/weather \
  MOCK_NOTE_ID=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  MOCK_TX_ID=0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  MOCK_SENDER=0x857b06519e91e3a54538791bdbb0e2 \
  MOCK_BLOCK_NUM=1 \
  pnpm --filter demo-agent start
```

Expected output: `status: 402` with `"error":"note not committed: 0xaaaa..."`
in the body. Swap in a real note id + tx id + sender + block from the
[`smoke-testnet`](../../docs/smoke-testnet.md) flow and the same run
returns `status: 200`.

## Adding a paywall to your own Express app

```ts
import express from 'express';
import { paywall } from '@miden-x402/merchant/express';

const app = express();

app.get(
  '/weather',
  paywall({
    facilitatorUrl: 'https://your-facilitator.example.com',
    price: {
      amount: '1000',
      asset: '0x0a7d175ed63ec5200fb2ced86f6aa5',
      payTo: '0x...your-merchant-account-id...',
      tokenSymbol: 'USDC',
      decimals: 6,
    },
  }),
  (_req, res) => res.json({ temperature: 21.5, city: 'Istanbul' }),
);

app.listen(3000);
```

That's the merchant integration in seven lines. The Hono variant is the
same shape — import from `@miden-x402/merchant/hono` instead.

### Private notes (M7)

Set `noteType: 'private'` on the `PriceTag` to opt into settled-at-commit
private P2ID. Same trust model, same end-to-end latency; only the
transaction-graph exposure on chain differs. The merchant code is
unchanged — the facilitator and agent SDKs handle the off-chain
`NoteFile` blob transport.

```ts
paywall({
  facilitatorUrl: 'https://your-facilitator.example.com',
  price: {
    amount: '1000',
    asset: '0x0a7d175ed63ec5200fb2ced86f6aa5',
    payTo: '0x...',
    tokenSymbol: 'USDC',
    decimals: 6,
    noteType: 'private',
  },
});
```

A live `/weather-private` route is wired in
[`examples/demo-merchant-express`](./examples/demo-merchant-express) and
[`examples/demo-merchant-hono`](./examples/demo-merchant-hono).

### Guardian verify-before-prove (M8)

Set `settlement: 'guardian-fast'` on the `PriceTag` to opt into the
Guardian flow (sub-second perceived latency, Guardian trust model). The
merchant SDK auto-calls the facilitator's `/guardian/challenge` endpoint
on the first request and inlines the server-issued `serial_num` into the
402 response; on the retry it forwards to `/guardian/settle`. Requires
the facilitator to be started with `MIDEN_X402_GUARDIAN_ENABLED=true`.

```ts
paywall({
  facilitatorUrl: 'https://guardian-facilitator.example.com',
  price: {
    amount: '1000',
    asset: '0x0a7d175ed63ec5200fb2ced86f6aa5',
    payTo: '0x...',
    tokenSymbol: 'USDC',
    decimals: 6,
    noteType: 'private',
    settlement: 'guardian-fast',
  },
});
```

For the agent side, the configured `Payer` must implement the optional
`payP2IDUnproven` method. The bundled `createWasmSdkPayer` delegates to a
`client.buildSignedUnprovenP2id` method on the underlying WASM SDK and
throws a clear `WasmSdkPayerError` if the installed `@miden-sdk/miden-sdk`
version does not yet expose it. See
[`docs/protocol.md`](../../docs/protocol.md) §A.2.7 for the full wire
contract.

## Implementing a custom `Payer`

The agent does not depend on `@miden-sdk/miden-sdk` directly; it accepts
any object implementing `Payer`. For settled-at-commit payments
(`'commit'` flow), implement `payP2ID`; for private notes also populate
`noteBlob` in the returned receipt. For the Guardian flow, optionally
implement `payP2IDUnproven`.

```ts
import { withMidenX402, type Payer } from '@miden-x402/agent';

const payer: Payer = {
  async payP2ID({ payTo, asset, amount, noteType }) {
    // your Miden client here
    return {
      noteId,
      transactionId,
      sender,
      blockNum,
      noteBlob, // required when noteType === 'private'
    };
  },

  // Optional — only needed if you support the Guardian flow.
  async payP2IDUnproven({ payTo, asset, amount, serialNum }) {
    // build P2ID with the server-issued serialNum, sign without proving
    return {
      txInputs,         // base64(TransactionInputs)
      signature,        // base64(Signature)
      signedSummary,    // base64(TransactionSummary)
      expectedNoteBlob, // base64(NoteFile::NoteDetails)
      transactionId,
      sender,
    };
  },
};

const fetchPaid = withMidenX402(fetch, { payer });
await fetchPaid('https://api.example.com/weather');
```
