/**
 * Demo agent CLI. Pays the demo merchant via the M4b agent wrapper.
 *
 * The note type (public / private) is chosen by the merchant via
 * `PaymentRequirements.extra.noteType`; the agent just forwards the choice
 * to the payer. Point `TARGET_URL` at `/weather` for public or
 * `/weather-private` for private.
 *
 * Two modes:
 *
 *   1. WASM-SDK mode (default once `@miden-sdk/miden-sdk` is installed):
 *      AGENT_BUYER=0x...  \
 *      AGENT_STORE_PATH=./buyer-store  \
 *      TARGET_URL=http://localhost:3000/weather  \
 *      pnpm --filter demo-agent start
 *
 *   2. Mock mode (for wiring tests, no real Miden network call):
 *      AGENT_MOCK=1 TARGET_URL=http://localhost:3000/weather \
 *      MOCK_NOTE_ID=0x...  MOCK_TX_ID=0x... MOCK_SENDER=0x...  \
 *      MOCK_BLOCK_NUM=1000 \
 *      [MOCK_NOTE_BLOB=...]   # required when hitting a private-note route
 *      pnpm --filter demo-agent start
 *
 * Mock mode lets you exercise the full Express/Hono merchant + facilitator
 * pipeline without needing a buyer account or WASM payload, by reusing a
 * pre-created note that the smoke-testnet binary already verified.
 */

import {
  createWasmSdkPayer,
  withMidenX402,
  type Payer,
  type P2idPaymentRequest,
  type P2idPaymentReceipt,
} from '@miden-x402/agent';

const TARGET_URL = required('TARGET_URL');

const payer: Payer = process.env.AGENT_MOCK
  ? mockPayer()
  : createWasmSdkPayer({
      buyerAccountId: required('AGENT_BUYER'),
      storePath: process.env.AGENT_STORE_PATH ?? './buyer-store',
    });

const x402Fetch = withMidenX402(fetch, {
  payer,
  onPaymentBuilt: (payload) => {
    console.log('paid: built signature for', payload.accepted.payTo);
  },
  onSettlement: (settle) => {
    console.log('settled:', settle.transaction, 'on', settle.network);
  },
});

(async () => {
  const response = await x402Fetch(TARGET_URL);
  console.log('status:', response.status);
  console.log('body:', await response.text());
})().catch((err) => {
  console.error(err);
  process.exit(1);
});

function required(key: string): string {
  const v = process.env[key];
  if (!v) {
    console.error(`missing required env var: ${key}`);
    process.exit(1);
  }
  return v;
}

function mockPayer(): Payer {
  return {
    async payP2ID(req: P2idPaymentRequest): Promise<P2idPaymentReceipt> {
      let noteBlob: string | undefined;
      if (req.noteType === 'private') {
        const blob = process.env.MOCK_NOTE_BLOB;
        if (!blob) {
          throw new Error(
            'agent mock: MOCK_NOTE_BLOB is required when the merchant requests noteType=private',
          );
        }
        noteBlob = blob;
      }
      return {
        noteId: required('MOCK_NOTE_ID'),
        transactionId: required('MOCK_TX_ID'),
        sender: required('MOCK_SENDER'),
        blockNum: Number(required('MOCK_BLOCK_NUM')),
        noteBlob,
      };
    },
  };
}
