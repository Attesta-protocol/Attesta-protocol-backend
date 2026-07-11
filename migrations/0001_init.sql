-- Attesta backend schema. Everything here is public chain state or ciphertext.
-- Invariant: no column ever stores a plaintext amount, spending key, or raw credential.

-- Per-pool indexer progress; rebuildable from chain events alone.
CREATE TABLE indexer_cursors (
    contract_id  TEXT PRIMARY KEY,
    last_ledger  BIGINT NOT NULL DEFAULT 0,
    last_cursor  TEXT,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Commitment tree leaves, mirrored from on-chain deposit/transfer events.
CREATE TABLE commitments (
    id          BIGSERIAL PRIMARY KEY,
    pool        TEXT   NOT NULL,
    leaf_index  BIGINT NOT NULL,
    commitment  BYTEA  NOT NULL,
    ledger      BIGINT NOT NULL,
    tx_hash     TEXT   NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pool, leaf_index),
    UNIQUE (pool, commitment)
);
CREATE INDEX commitments_pool_idx ON commitments (pool, leaf_index);

-- Spent-note nullifiers, mirrored from transfer/withdraw events.
CREATE TABLE nullifiers (
    id         BIGSERIAL PRIMARY KEY,
    pool       TEXT   NOT NULL,
    nullifier  BYTEA  NOT NULL,
    ledger     BIGINT NOT NULL,
    tx_hash    TEXT   NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pool, nullifier)
);

-- Encrypted note blobs emitted on transfers. The relay sees only ciphertext;
-- recipients trial-decrypt client-side with their viewing keys.
CREATE TABLE encrypted_notes (
    id                BIGSERIAL PRIMARY KEY, -- doubles as the pagination cursor
    pool              TEXT  NOT NULL,
    commitment        BYTEA NOT NULL,
    ephemeral_pubkey  BYTEA NOT NULL,
    ciphertext        BYTEA NOT NULL,
    ledger            BIGINT NOT NULL,
    tx_hash           TEXT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX encrypted_notes_pool_id_idx ON encrypted_notes (pool, id);

-- Mirror of the on-chain issuer registry (public state).
CREATE TABLE issuers (
    issuer_id    TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    public_key   BYTEA NOT NULL,
    claim_types  TEXT[] NOT NULL DEFAULT '{}',
    status       TEXT NOT NULL DEFAULT 'active', -- active | suspended | revoked
    registered_ledger BIGINT NOT NULL DEFAULT 0,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Issuer-gateway credential deliveries. The credential payload is encrypted
-- to the recipient before it reaches this service; we store ciphertext only.
CREATE TABLE credential_deliveries (
    delivery_id     UUID PRIMARY KEY,
    issuer_id       TEXT NOT NULL REFERENCES issuers (issuer_id),
    recipient_hint  TEXT NOT NULL, -- opaque, recipient-derived mailbox tag (not an identity)
    ciphertext      BYTEA NOT NULL,
    issuer_signature BYTEA NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    claimed_at      TIMESTAMPTZ
);
CREATE INDEX credential_deliveries_hint_idx ON credential_deliveries (recipient_hint);

-- Public pool totals (TVL is public by construction: deposits and
-- withdrawals cross the shielded boundary with visible amounts).
CREATE TABLE pool_totals (
    pool         TEXT PRIMARY KEY,
    asset        TEXT NOT NULL,
    total_in     NUMERIC(39, 0) NOT NULL DEFAULT 0,
    total_out    NUMERIC(39, 0) NOT NULL DEFAULT 0,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
