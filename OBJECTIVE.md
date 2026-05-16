# Building Agentic Payments Support on Miden

## Overview

This doc suggests what to build and why. The goal is native support for the primary agentic payment protocols on Miden, positioning Miden as a privacy-preserving settlement layer for autonomous agent transactions, with Guardian-enforced authorization as the technical differentiator.

## Background: three categories of agentic payments

The agentic payments space spans three categories with very different dynamics. Understanding which one we're targeting matters for design decisions.

**Agent-to-Service (M2M).** An agent calls an API and pays per request. Currently the most active category. Coinbase reports 140M x402 transactions. MPP launched March 2026 with 100+ adopters including Anthropic, OpenAI, Shopify, Alchemy, Dune, Browserbase, Parallel Web Systems. AWS shipped AgentCore Payments and CloudFront x402 reference architecture in March. Sub-dollar transactions, sub-second settlement, machines on both sides.

**Agent-to-Merchant (e-commerce).** Agent buys consumer products from merchants. Live via ChatGPT Instant Checkout with Walmart, Target, Sephora and others. Flows through ACP and settles in fiat via Stripe. The merchant is locked to their existing payment processor. Hard to get it tbh, because Stripe settles crypto vs. Fiat in the end and they won't accept Miden payments (today).

**Agent-to-Agent.** Two autonomous agents transacting directly. Mostly aspirational today. Real production volumes are not reported. Not realistic future bs.

**We're targeting M2M.** Reasons: real volume today, settlement choice is genuinely open (API providers choose what they accept, unlike merchants who are locked to Stripe), privacy actually matters (agent API call patterns reveal strategy and customer lists), and buyers are addressable.

### For the future, we also target e-commerce. 

The full agentic commerce stack has four layers. We touch two.

1. **Commerce layer.** Product discovery, cart, checkout. Owned by ACP (OpenAI/Stripe) and UCP (Google/Shopify). Off-chain. Not relevant.
2. **Authorization layer.** Mandates that authorize an agent to act on behalf of a user. Owned by AP2. This is where Guardian fits.
3. **Payment layer.** HTTP 402 negotiation, signed credentials, receipts. Owned by x402 and MPP.
4. **Settlement layer.** Where the money actually lands. Tempo, Base, Solana, Miden, or fiat via Stripe.

Our work lives at layers 2 (Guardian-as-mandate via AP2) and 4 (Miden as settlement chain). The x402 and MPP work at layer 3 is the integration glue that makes layer 4 reachable.

## Scope of work

Four deliverables, in priority order.

### 1. x402 facilitator on Miden

x402 is the crypto-native HTTP 402 protocol from Coinbase, open-sourced under Apache 2.0.

Flow:
1. Agent requests a resource from a paid endpoint
2. Server responds with 402 Payment Required plus payment details
3. Agent constructs a payment payload, resends the request with a PAYMENT-SIGNATURE header
4. Facilitator (our work) verifies the payment on Miden
5. Server delivers the resource

Build:
- x402 facilitator that verifies USDC payments on Miden (USDM as fallback)
- Reference merchant endpoint (Python and Node) that integrates with the facilitator
- Reference agent client that pays the merchant
- Working end-to-end demo: agent pays a test API service privately, receipt is verifiable, transaction graph is not exposed

### 2. MPP integration

MPP is Stripe and Tempo's machine payments protocol. Similar 402-based flow, broader payment method support, on the IETF track.

Three implementation levels exist in the spec:
- One-shot payments (tagged stablecoin transfer with challenge ID)
- Session-based streaming via onchain escrow contracts (state channels)
- Gasless execution via relayed transactions

For this initial work, build level 1 only. Onchain escrow for streaming vouchers is significant additional engineering and is out of scope for now.

Build:
- MPP one-shot payment support on Miden
- SDK skeleton in TypeScript and Python (these are the mppx target languages)
- Documentation showing how to integrate

### 3. AP2 + Guardian integration

AP2 is Google's agent payments protocol. It defines verifiable digital credentials that encode delegated authority: spend limits, time windows, merchant allowlists, transaction categories. The standard implementation pattern is "merchant verifies the mandate signature, decides whether to accept."

Our implementation pattern is different and is the technical wedge: Guardian co-signs every transaction, and the Guardian signing policy directly encodes AP2 mandate conditions. Authorization is enforced at the chain layer, not the application layer.

What this means in practice:
- A Miden transaction without valid Guardian co-signature does not settle
- Mandate violations are rejected by the chain, not by the merchant
- The mandate chain (user signed mandate, then Guardian-enforced policy, then transaction) is auditable end-to-end
- Privacy and mandate enforcement compose, because Guardian sees what others don't

Build:
- AP2 mandate parsing on Miden (W3C VDC standard)
- Guardian policy module that ingests an AP2 mandate and translates it to a signing policy
- Reference flow: user signs AP2 mandate, agent constructs transaction within mandate bounds, Guardian co-signs, transaction settles. Same flow attempted outside mandate bounds is rejected by Guardian.

### 4. Benchmarks

Once the above is working, produce a benchmark matrix comparing Miden against the main alternatives.

Chains to benchmark:
- Miden (our implementation)
- Tempo (Stripe and Paradigm's payments-first L1, mainnet live March 2026)
- Base (Coinbase L2, the default x402 settlement chain)
- MultiversX (shipped MPP integration with three-tier architecture in March)

Metrics to measure:
- Transactions per second under sustained agent workload
- End-to-end latency from 402 response to receipt
- Cost per transaction in USD
- Privacy properties: what's visible onchain, and to whom

Workload should be a realistic M2M pattern, not synthetic stress testing. Suggested reference workload: an agent making sequential API calls of varying cost, mixed payment methods, occasional mandate validation.

## Why this matters: the settlement adoption challenge

Important context for design decisions.

Today's M2M flow: agent pays in USDC on Base via x402, or pays via MPP with Stripe converting to fiat at the boundary. The API provider gets paid in their existing rail and doesn't care which chain settled it.

For Miden to actually be a settlement layer, three things need to work:
1. API providers must accept Miden settlement specifically, not just "stablecoin"
2. A facilitator must verify Miden payments (this is what we're building)
3. Agent infrastructure (Crossmint, Skyfire, Payman, Nevermined) must be able to construct Miden transactions

**Implication for design: the SDKs and facilitator need to be drop-in easy.** If integrating Miden takes more effort than integrating Base, providers won't bother. Target: an API provider should be able to add Miden as an accepted settlement chain in under an hour, given they already accept x402 elsewhere.

The privacy story is what gets providers to consider Miden in the first place. The integration ease is what gets them to actually ship.

## Out of scope

Explicit list to avoid ambiguity:
- ACP and UCP merchant checkout protocols
- Google A2A agent coordination protocol
- ERC-8004 agent identity registry
- MPP streaming vouchers and onchain escrow contracts
- Card or Lightning payment method support in MPP
- Frontend or dashboard work

## References

- x402 spec: x402.org
- x402 reference implementation: github.com/coinbase/x402
- MPP announcement and integration guide: stripe.com/blog/machine-payments-protocol
- AP2: Google's agent payments protocol (W3C Verifiable Credentials based)
- MultiversX MPP implementation (closest comparable, useful for benchmarking and design reference): multiversx.com/blog/stripes-machine-payments-protocol-on-multiversx
- AWS x402 reference architecture: aws.amazon.com/blogs/industries/x402-and-agentic-commerce-redefining-autonomous-payments-in-financial-services