---
name: verify
description: Build, launch, and drive the Attesta backend (API + indexer) against local Postgres and a mock Soroban RPC to verify changes end-to-end.
---

# Verifying the Attesta backend

## Build & launch

```bash
docker compose up -d db     # Postgres 16 on :5432 (user/pass/db: attesta)
cargo build --bin api --bin indexer
```

**Gotcha:** always `cargo build` before running `./target/debug/<bin>` —
`cargo test`/`clippy` do NOT refresh the normal bin artifacts, so you can
end up driving a stale binary. `cargo run --bin …` is safe but spawns the
binary as a child that survives `timeout`; prefer running the built binary
directly with `nohup … &` and killing it by PID.

API (runs migrations on start; no secrets needed):

```bash
DATABASE_URL=postgres://attesta:attesta@localhost:5432/attesta \
BIND_ADDR=127.0.0.1:8899 ./target/debug/api
```

## Driving the indexer without a real chain

The indexer only needs a JSON-RPC endpoint answering `getLatestLedger` and
`getEvents`. Serve base64-XDR `ScVal` events from a small local mock
(hand-encode XDR: u32 BE discriminant then payload; Symbol=15, Bytes=13,
U32=3, I128=10, String=14, Vec=16, Map=17; Vec/Map are `Some`-wrapped so
prefix with u32(1); the provisional event layout is documented in
`crates/attesta-indexer/src/events.rs`). Then:

```bash
DATABASE_URL=postgres://attesta:attesta@localhost:5432/attesta \
SOROBAN_RPC_URL=http://127.0.0.1:8799 \
POOL_CONTRACT_IDS=CPOOLTEST REGISTRY_CONTRACT_ID=CREGTEST \
INDEXER_POLL_SECS=2 ./target/debug/indexer
```

Have the mock respect `params.startLedger` (return no events below it) or
the sync loop re-fetches forever. Idempotent inserts make replays harmless.

## Flows worth driving

- `GET /health/live` + `/health/ready` (stop the db container → ready
  flips to 503 `{"failing":"database"}` and recovers without a restart),
  `GET /v1/tree/{pool}/root`, `GET /v1/tree/{pool}/path?commitment=0x…`
- Recompute the served Merkle path externally (sha256 over
  `left||right`, `sibling_on_right` chooses order) — must equal `root`.
- Historical roots: after seeding leaves at distinct ledgers,
  `/root?at_ledger=L` and `/root?at_leaf_count=N` must return the
  matching `tree_roots` row; both params together → 400.
- Tree cache top-up: INSERT a commitment row mid-session, re-hit `/root`,
  leaf_count must grow without an API restart (and `tree_roots` gains a
  row per new leaf).
- Gap guard: INSERT a leaf with a skipped `leaf_index` — tree must stop
  before the gap and log "gap in commitment leaf indexes".
- Claim lifecycle: deliver with `claim_token_hash` (sha256 of the token,
  b64) → pickup shows it → claim with wrong token 403, right token 204,
  again 409 → pickup no longer returns it.
- SSE resume: seed notes, connect with `Last-Event-ID: N` → replays
  `id > N` in order, then a fresh INSERT arrives live via LISTEN/NOTIFY
  (log line "note fan-out in push mode").
- Rate limits: >RATE_LIMIT_WRITE_BURST rapid POSTs → 429 with
  `Retry-After`, while reads keep returning 200.
- `GET /metrics`: `attesta_api_requests_total` labeled by route pattern
  (never raw URLs), tree/SSE gauges present.
- Indexer decode: check `commitments`, `nullifiers`, `encrypted_notes`,
  `pool_totals`, `issuers`, `indexer_cursors` tables after a sync pass.

## Cleanup

```bash
docker compose exec -T db psql -U attesta -d attesta -c \
  "TRUNCATE commitments, nullifiers, encrypted_notes, pool_totals, issuers, indexer_cursors, credential_deliveries, tree_roots CASCADE;"
docker compose stop db
```
