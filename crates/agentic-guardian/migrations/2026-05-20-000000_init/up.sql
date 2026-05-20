-- agentic-guardian initial schema.
--
-- Per ideas/NEW_DESIGN.md the agentic-guardian persists six kinds of
-- records: agents, mandates, pending_states, reservations, batch_queue,
-- challenges, mandate_counters.

CREATE TABLE agents (
    agent_account_id TEXT PRIMARY KEY,
    hot_pubkey_commitment_hex TEXT NOT NULL,
    cold_pubkey_commitment_hex TEXT NOT NULL,
    registered_at_unix_secs BIGINT NOT NULL
);

CREATE TABLE mandates (
    mandate_id TEXT PRIMARY KEY,
    agent_account_id TEXT NOT NULL REFERENCES agents(agent_account_id) ON DELETE CASCADE,
    signed_payload_json JSONB NOT NULL,
    stored_at_unix_secs BIGINT NOT NULL
);
CREATE INDEX mandates_by_agent ON mandates (agent_account_id);

CREATE TABLE pending_states (
    agent_account_id TEXT PRIMARY KEY REFERENCES agents(agent_account_id) ON DELETE CASCADE,
    current_commitment_hex TEXT NOT NULL,
    nonce BIGINT NOT NULL,
    last_advanced_at_unix_secs BIGINT NOT NULL
);

CREATE TABLE reservations (
    nullifier_hex TEXT PRIMARY KEY,
    owning_queued_id TEXT NOT NULL,
    reserved_at_unix_secs BIGINT NOT NULL,
    expires_at_unix_secs BIGINT NOT NULL,
    promoted BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX reservations_by_owner ON reservations (owning_queued_id);
CREATE INDEX reservations_by_expiry ON reservations (expires_at_unix_secs);

CREATE TABLE batch_queue (
    queued_id TEXT PRIMARY KEY,
    agent_account_id TEXT NOT NULL,
    mandate_id TEXT NOT NULL,
    serial_num TEXT NOT NULL,
    payer TEXT NOT NULL,
    tx_inputs_b64 TEXT NOT NULL,
    hot_signature_b64 TEXT NOT NULL,
    signed_summary_b64 TEXT NOT NULL,
    network TEXT NOT NULL,
    enqueued_at_unix_secs BIGINT NOT NULL,
    submitted BOOLEAN NOT NULL DEFAULT FALSE,
    on_chain_tx_id TEXT
);
CREATE INDEX batch_queue_by_enqueued_at ON batch_queue (enqueued_at_unix_secs);
CREATE INDEX batch_queue_unsubmitted ON batch_queue (submitted) WHERE submitted = FALSE;

CREATE TABLE challenges (
    serial_num TEXT PRIMARY KEY,
    requirements_json JSONB NOT NULL,
    issued_at_unix_secs BIGINT NOT NULL,
    expires_at_unix_secs BIGINT NOT NULL
);
CREATE INDEX challenges_by_expiry ON challenges (expires_at_unix_secs);

CREATE TABLE mandate_counters (
    agent_account_id TEXT NOT NULL,
    window_start_unix_secs BIGINT NOT NULL,
    total_amount BIGINT NOT NULL,
    PRIMARY KEY (agent_account_id, window_start_unix_secs)
);
CREATE INDEX mandate_counters_by_agent_time ON mandate_counters (agent_account_id, window_start_unix_secs DESC);
