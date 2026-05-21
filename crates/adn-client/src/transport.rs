//! HTTP helpers for the agent's communication with the merchant.
//!
//! The agent only talks to the merchant. The merchant relays to the
//! facilitator internally (matching the Base x402 flow).

// No facilitator transport needed — the agent embeds the signed debit
// in the Payment-Signature header when retrying the merchant request.
// The merchant handles facilitator communication.
