//! Diesel schema for the agentic-guardian tables.
//!
//! Normally Diesel generates this with `diesel print-schema`. We commit
//! it by hand here so the crate compiles without a live database; CI
//! runs `diesel migration run` then `diesel print-schema --check` to
//! catch drift.
//!
//! Reference migrations: `crates/agentic-guardian/migrations/`.

#![allow(clippy::missing_docs_in_private_items)]

use diesel::table;

table! {
    agents (agent_account_id) {
        agent_account_id -> Text,
        hot_pubkey_commitment_hex -> Text,
        cold_pubkey_commitment_hex -> Text,
        registered_at_unix_secs -> Int8,
    }
}

table! {
    mandates (mandate_id) {
        mandate_id -> Text,
        agent_account_id -> Text,
        signed_payload_json -> Jsonb,
        stored_at_unix_secs -> Int8,
    }
}

table! {
    pending_states (agent_account_id) {
        agent_account_id -> Text,
        current_commitment_hex -> Text,
        nonce -> Int8,
        last_advanced_at_unix_secs -> Int8,
    }
}

table! {
    reservations (nullifier_hex) {
        nullifier_hex -> Text,
        owning_queued_id -> Text,
        reserved_at_unix_secs -> Int8,
        expires_at_unix_secs -> Int8,
        promoted -> Bool,
    }
}

table! {
    batch_queue (queued_id) {
        queued_id -> Text,
        agent_account_id -> Text,
        mandate_id -> Text,
        serial_num -> Text,
        payer -> Text,
        tx_inputs_b64 -> Text,
        hot_signature_b64 -> Text,
        signed_summary_b64 -> Text,
        network -> Text,
        enqueued_at_unix_secs -> Int8,
        submitted -> Bool,
        on_chain_tx_id -> Nullable<Text>,
    }
}

table! {
    challenges (serial_num) {
        serial_num -> Text,
        requirements_json -> Jsonb,
        issued_at_unix_secs -> Int8,
        expires_at_unix_secs -> Int8,
    }
}

table! {
    mandate_counters (agent_account_id, window_start_unix_secs) {
        agent_account_id -> Text,
        window_start_unix_secs -> Int8,
        total_amount -> Int8,
    }
}
