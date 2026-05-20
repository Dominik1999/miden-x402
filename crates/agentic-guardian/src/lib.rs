//! Agentic-guardian — high-throughput verify-before-prove + batched
//! settlement server per [`ideas/NEW_DESIGN.md`].
//!
//! This crate is the **counter-proposal branch** to the OZ-Guardian-bolted-on
//! refactor on `main`. It implements:
//!
//! - An agent account model with a **hot key** (free in-mandate signing) and
//!   a **cold key** (Guardian co-sig for out-of-mandate ops).
//! - **AP2 mandate enforcement**: per-tx amount cap, merchant allowlist,
//!   rolling time window, daily total cap.
//! - **Per-agent pending state tracking** — the Guardian is the single
//!   serialization point, so the agent can submit multiple in-flight txs
//!   that chain on each other.
//! - **Batched STARK proving** — verified txs are queued, proved in
//!   parallel (one `RemoteTransactionProver::prove` per tx), then
//!   assembled into a `TransactionBatch` and submitted via the Miden
//!   node's `SubmitProvenBatch` RPC.
//! - **Crash recovery via Postgres transactions** — reservations + queue
//!   are durable; on restart the in-flight set is replayed.
//!
//! Architecture inspired by the [`inicio-labs/MultiSig`](https://github.com/inicio-labs/MultiSig/tree/hubcycle-ui-flow-md)
//! coordinator-server: Axum REST + Diesel/Postgres + a `!Send + !Sync`
//! `MidenAgenticClientRuntime` running the underlying `miden-client` on
//! a dedicated thread via `tokio::task::LocalSet` and mpsc/oneshot
//! message-passing.

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod batch;
pub mod config;
pub mod error;
pub mod mandate;
pub mod recovery;
pub mod runtime;
pub mod state;
pub mod storage;

pub use config::{Config, ConfigError};
pub use error::{AgenticError, AgenticResult};
