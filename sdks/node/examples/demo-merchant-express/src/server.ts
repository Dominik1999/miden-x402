/**
 * Demo merchant gated by Miden x402. Run with:
 *
 *   MERCHANT_PAY_TO=0x...  \
 *   ASSET=0x0a7d175ed63ec5200fb2ced86f6aa5  \
 *   FACILITATOR_URL=http://localhost:8080  \
 *   pnpm --filter demo-merchant-express start
 *
 * Then GET http://localhost:3000/weather to receive a 402.
 */

import express from 'express';

import { paywall } from '@miden-x402/merchant/express';

const PORT = Number(process.env.PORT ?? 3000);
const PAY_TO = required('MERCHANT_PAY_TO');
const ASSET = process.env.ASSET ?? '0x0a7d175ed63ec5200fb2ced86f6aa5';
const FACILITATOR_URL = process.env.FACILITATOR_URL ?? 'http://localhost:8080';
const AMOUNT = process.env.AMOUNT ?? '1000';

const app = express();

app.get(
  '/weather',
  paywall({
    facilitatorUrl: FACILITATOR_URL,
    price: {
      amount: AMOUNT,
      asset: ASSET,
      payTo: PAY_TO,
      tokenSymbol: 'USDC',
      decimals: 6,
    },
    description: 'current weather (public note)',
    mimeType: 'application/json',
  }),
  (_req, res) => {
    res.json({ temperature: 21.5, city: 'Istanbul' });
  },
);

// Private-note variant of the same paywall. Same merchant code path; only
// the `PriceTag.noteType` differs. The agent picks up `noteType: 'private'`
// from the merchant's PaymentRequired and routes the payer accordingly.
app.get(
  '/weather-private',
  paywall({
    facilitatorUrl: FACILITATOR_URL,
    price: {
      amount: AMOUNT,
      asset: ASSET,
      payTo: PAY_TO,
      tokenSymbol: 'USDC',
      decimals: 6,
      noteType: 'private',
    },
    description: 'current weather (private note)',
    mimeType: 'application/json',
  }),
  (_req, res) => {
    res.json({ temperature: 21.5, city: 'Istanbul' });
  },
);

app.listen(PORT, () => {
  console.log(`demo-merchant-express listening on http://localhost:${PORT}`);
  console.log(`  payTo=${PAY_TO}`);
  console.log(`  asset=${ASSET}`);
  console.log(`  amount=${AMOUNT}`);
  console.log(`  facilitator=${FACILITATOR_URL}`);
  console.log(`  routes: /weather (public)  /weather-private (private)`);
});

function required(key: string): string {
  const v = process.env[key];
  if (!v) {
    console.error(`missing required env var: ${key}`);
    process.exit(1);
  }
  return v;
}
