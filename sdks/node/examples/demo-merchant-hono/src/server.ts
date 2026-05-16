/**
 * Hono variant of the demo merchant. See the Express one for env config.
 */

import { serve } from '@hono/node-server';
import { Hono } from 'hono';

import { paywall } from '@miden-x402/merchant/hono';

const PORT = Number(process.env.PORT ?? 3000);
const PAY_TO = required('MERCHANT_PAY_TO');
const ASSET = process.env.ASSET ?? '0x0a7d175ed63ec5200fb2ced86f6aa5';
const FACILITATOR_URL = process.env.FACILITATOR_URL ?? 'http://localhost:8080';
const AMOUNT = process.env.AMOUNT ?? '1000';

const app = new Hono();

app.use(
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
    description: 'current weather',
    mimeType: 'application/json',
  }),
);

app.get('/weather', (c) => c.json({ temperature: 21.5, city: 'Istanbul' }));

serve({ fetch: app.fetch, port: PORT }, () => {
  console.log(`demo-merchant-hono listening on http://localhost:${PORT}`);
  console.log(`  payTo=${PAY_TO}`);
  console.log(`  asset=${ASSET}`);
  console.log(`  amount=${AMOUNT}`);
  console.log(`  facilitator=${FACILITATOR_URL}`);
});

function required(key: string): string {
  const v = process.env[key];
  if (!v) {
    console.error(`missing required env var: ${key}`);
    process.exit(1);
  }
  return v;
}
