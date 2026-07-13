-- Historical root anchoring (Issue 5). The tree is append-only, so the
-- root after leaf n is deterministic; one row per append is cheap and
-- fully replayable from the commitments table (drop-the-database recovery
-- still holds). The API owns root computation: it persists a row for each
-- leaf it appends to its in-memory tree during top-up.
CREATE TABLE tree_roots (
    id          BIGSERIAL PRIMARY KEY,
    pool        TEXT   NOT NULL,
    leaf_count  BIGINT NOT NULL, -- tree size after this append (leaf_index + 1)
    root        BYTEA  NOT NULL,
    ledger      BIGINT NOT NULL, -- ledger of the leaf that produced this root
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pool, leaf_count)
);
-- "newest root at or before ledger L" lookups.
CREATE INDEX tree_roots_pool_ledger_idx ON tree_roots (pool, ledger, leaf_count);
