# Attesta Backend — Issue Backlog

Ten proposed issues toward the M2–M5 milestones (5–9 are now implemented; status notes inline), grounded in the current
code. Each is written to be copy-pasted into the tracker as-is. Labels
follow the conventions in CONTRIBUTING: `security-critical` items require
dual review; the no-secrets-server invariant applies to every issue here.

Ordering is by suggested priority within each milestone, not strictly by
number.

---

## Issue 1 — Poseidon-over-BLS12-381 tree hasher matching the circuits

**Labels:** `backend`, `security-critical`, `M2`
**Blocked on:** circuit repository publishing its Poseidon instance parameters

### Description

The commitment Merkle tree (`crates/attesta-core/src/merkle.rs`) currently
hashes with SHA-256 behind the `TreeHasher` trait. This is an explicit
placeholder: the tree served to provers **must** produce the same roots and
paths as the tree the circuits constrain, which is a Poseidon instance over
the BLS12-381 scalar field (the curve Soroban exposes since Protocol 25).
Until the hasher is swapped, paths served by `/v1/tree/{pool}/path` cannot
be used in a real proof.

The `TreeHasher` abstraction was designed so this lands as a new impl plus
a one-line wiring change — but because a wrong constant here produces
undetectable soundness bugs (client proofs verify against the wrong tree,
or never verify at all), the parameters must be imported from the circuit
repo, never re-derived by hand.

### Tasks

- [ ] Add a `PoseidonHasher` implementing `TreeHasher`, using an audited
      arkworks-compatible Poseidon implementation over the BLS12-381 scalar
      field
- [ ] Import round constants, MDS matrix, and rate/capacity directly from
      the circuit repository's parameter files (vendored with a checksum,
      not copied by hand)
- [ ] Define the leaf/empty-leaf domain separation to match the circuit
      (`empty_leaf()` must equal the circuit's zero-leaf constant)
- [ ] Handle canonical field encoding: commitments arrive as 32 bytes;
      reject or reduce non-canonical scalars the same way the circuit does
- [ ] Add cross-implementation test vectors: hash outputs, empty-subtree
      roots per depth, and a full 8-leaf tree root, all asserted against
      vectors exported from the circuit repo
- [ ] Wire the hasher choice through config so testnet pools can keep
      SHA-256 until their contracts migrate
- [ ] Update `merkle.rs` module docs and the README status section

### Acceptance criteria

- [ ] `MerkleTree<PoseidonHasher>` reproduces the circuit repo's exported
      test vectors bit-for-bit (leaf hash, `hash_pair`, depth-0..32 zero
      roots, sample tree root and path)
- [ ] A path served by the API verifies inside the actual circuit (one
      end-to-end proof on testnet, documented in the PR)
- [ ] No hand-transcribed constants: parameters are loaded from a vendored
      file whose SHA-256 is asserted in a test
- [ ] Dual review recorded on the PR (two approving reviews, per
      `security-critical` policy)

---

## Issue 2 — Freeze the on-chain event layout and gate the JSON shim out of production builds

**Labels:** `backend`, `indexer`, `M2`
**Blocked on:** pool/registry contracts deploying to testnet

### Description

`crates/attesta-indexer/src/events.rs` decodes a *provisional* XDR layout
(symbol-keyed `ScMap` payloads, documented in the module header) and keeps
a JSON shim fallback so tests can drive ingest with plain-text events.
Two risks remain:

1. When the real contracts land, their event layout may diverge from the
   provisional one (different field names, tuple-style `ScVec` payloads
   instead of maps, multi-topic events). Divergence would silently drop
   every event (`decode` returns `None` and only logs).
2. The JSON shim is compiled into production binaries. It is only
   reachable if an RPC returned non-XDR values — which no real RPC does —
   but a decoding path that accepts unauthenticated plain-text input has
   no business existing outside tests.

### Tasks

- [ ] Once the contracts are on testnet, capture real `getEvents` payloads
      for every event type and check them into
      `crates/attesta-indexer/testdata/` as fixtures
- [ ] Reconcile `events.rs` field tables with the frozen layout; delete the
      "provisional" language from the module docs
- [ ] Move the JSON shim behind `#[cfg(test)]` (or a `test-shim` cargo
      feature) so release builds contain only the XDR path
- [ ] Add a conformance test that decodes every checked-in fixture and
      asserts the full typed result
- [ ] Add a counter/log for *recognized-but-undecodable* events so layout
      drift is loud, not silent (see also Issue 9 for the metric)

### Acceptance criteria

- [ ] Every fixture captured from the deployed contracts decodes to the
      expected `PoolEvent` in `cargo test`
- [ ] `cargo build --release` output contains no JSON shim code path
      (verified by feature-gating; a test asserts the shim is cfg'd out)
- [ ] Indexing a testnet pool from ledger 0 populates `commitments`,
      `nullifiers`, `encrypted_notes`, `pool_totals`, and `issuers` with
      counts matching the contract's own storage
- [ ] A deliberately malformed event increments the undecodable counter
      and does not stall the ingest loop

---

## Issue 3 — Verify issuer signatures on credential delivery

**Labels:** `backend`, `api`, `security-critical`, `M5`
**Blocked on:** credential envelope format (M5)

### Description

`POST /v1/issuer/credentials` (`crates/attesta-api/src/routes/issuer.rs`)
accepts a base64 `issuer_signature` and stores it, but never verifies it —
there is an explicit `TODO(M5)` where the check belongs. Today anyone who
knows an active `issuer_id` can stuff arbitrary ciphertext into any
recipient's mailbox, attributed to that issuer. Recipients would fail to
decrypt or validate it client-side, so no *secret* is at risk, but the
gateway currently launders spam under real issuer names and lets
`credential_deliveries` grow with garbage.

The registry mirror already stores each issuer's `public_key` (mirrored
from chain by the indexer), so the backend has everything it needs except
the finalized envelope definition: exactly which bytes the issuer signs
(ciphertext alone, or a domain-separated digest including `recipient_hint`
and issuer id — the latter prevents replaying one signed blob to a
different mailbox).

### Tasks

- [ ] Specify the signed message: recommend
      `H(domain_tag || issuer_id || recipient_hint || ciphertext)` and
      document it in `docs/credential-format.md`
- [ ] Pin the signature scheme (Ed25519 to match Stellar keys) and key
      encoding for `issuers.public_key`
- [ ] Implement verification in `deliver_credential` before the INSERT;
      reject with 401 and a machine-readable error code on failure
- [ ] Decide replay policy: reject exact duplicate
      `(issuer_id, recipient_hint, ciphertext)` tuples via a unique index
      or content hash column (new migration)
- [ ] Add unit tests: valid signature accepted; wrong key, wrong message,
      truncated signature, and cross-mailbox replay all rejected
- [ ] Update the README API section and the no-secrets invariant note
      (signature covers ciphertext only — never plaintext)

### Acceptance criteria

- [ ] A delivery signed by the registered issuer key is stored and
      retrievable via `GET /v1/credentials`
- [ ] A delivery with an invalid/missing/replayed signature is rejected
      with 401/409 and **no row is written**
- [ ] Suspended or revoked issuers cannot deliver (already enforced for
      unknown issuers; extend the test matrix)
- [ ] Dual review recorded (security-critical surface)

---

## Issue 4 — Disclosure CLI: trial decryption and per-entry Merkle proof verification

**Labels:** `backend`, `disclosure`, `M4`
**Blocked on:** note encryption format (M3)

### Description

`cargo run --bin disclosure -- generate` currently pages through every
encrypted note and writes a report with an **empty `entries` array** —
the `TODO(M4)` in `crates/attesta-disclosure/src/main.rs`. The whole value
of the tool is the missing part: trial-decrypting notes with the auditor's
*scoped viewing key* (which never leaves the machine) and emitting, per
decrypted note, the amount/memo plus the Merkle path proving that note's
commitment is in the pool tree.

`verify` is similarly a stub: it compares the report's root against the
*current* live root, which spuriously fails as soon as one new deposit
lands. Real verification re-checks each entry's Merkle path against the
root the report was anchored to (see Issue 5 for the historical-root API
it needs).

### Tasks

- [ ] Implement the M3 note envelope decryption (X25519 ECDH against the
      note's `ephemeral_pubkey` + AEAD, per the frozen M3 spec) behind a
      `NoteDecryptor` trait so the format can evolve
- [ ] Trial-decrypt every fetched note; collect successes with amount,
      asset, memo, commitment, leaf index, ledger, and tx hash
- [ ] For each success, fetch `/v1/tree/{pool}/path?commitment=…` and embed
      the path + root + anchored ledger in the report entry
- [ ] Re-verify each embedded path locally before writing the report
      (reuse `attesta_core::merkle::verify_path`)
- [ ] Rewrite `verify` to recompute every entry's path→root and check the
      root against the anchored ledger via the historical-root endpoint
      (Issue 5), falling back to current-root comparison with a warning
- [ ] Zeroize the viewing key buffer after use (`zeroize` crate); never
      log key material or decrypted amounts at any level
- [ ] Add tests with fixture notes encrypted to a test viewing key: right
      key decrypts N entries, wrong key decrypts zero, tampered ciphertext
      is skipped without panicking

### Acceptance criteria

- [ ] `generate` against a seeded backend produces a report whose entries
      exactly match the notes encrypted to the test viewing key — no more,
      no fewer
- [ ] Every report entry carries a Merkle path that `verify` re-validates
      offline against the anchored root
- [ ] `verify` passes on an old report after new deposits have changed the
      live root (anchored verification, not live-root equality)
- [ ] Running `generate` with the wrong viewing key yields
      `entries: []` and exit code 0 (scoping works; not an error)
- [ ] `grep`-audit shows no code path logging key bytes or plaintext
      amounts (invariant holds)

---

## Issue 5 — Historical root anchoring: root history table and `/v1/tree/{pool}/root?at_ledger=` support

**Status: implemented.** API owns root computation (persists a row per appended leaf during tree top-up, batched); `/root?at_ledger=` and `?at_leaf_count=` answer from `tree_roots` by index lookup; path responses carry `leaf_count`. Backfill is automatic: a fresh API process replays all leaves on first tree request.

**Labels:** `backend`, `api`, `indexer`, `M2`

### Description

Proofs are generated against a root that is immediately superseded by the
next deposit. The pool contract accepts recent historical roots for
exactly this reason — but the backend only serves the *current* root.
Two consumers need history:

1. **Provers** need "root as of ledger L" to know whether their proof's
   anchor is still within the contract's accepted window.
2. **Disclosure verification** (Issue 4) needs to re-check a report
   against the root that existed at `anchored_ledger`, not today's root.

Since the tree is append-only, the root after leaf *n* is deterministic;
storing `(pool, leaf_count, root, ledger)` per append (or per ledger with
appends) is cheap and fully replayable from chain events, preserving the
drop-the-database recovery property.

### Tasks

- [ ] Migration: `tree_roots (pool, leaf_count, root BYTEA, ledger,
      UNIQUE(pool, leaf_count))`
- [ ] Indexer writes a root row per ingested commitment batch (compute
      via the shared incremental tree, or have the API backfill from
      leaves — decide and document which process owns root computation)
- [ ] Extend `GET /v1/tree/{pool}/root` with optional `at_ledger=` /
      `at_leaf_count=` query params returning the newest root at or before
      that point
- [ ] Return `leaf_count` alongside every path response so clients can
      pin path ↔ root ↔ ledger consistently (path is already served from
      one consistent in-memory snapshot)
- [ ] Backfill task for existing deployments (replay from `commitments`)
- [ ] Tests: root history matches recomputed roots for a random append
      sequence; `at_ledger` between two appends returns the earlier root

### Acceptance criteria

- [ ] `GET /v1/tree/{pool}/root?at_ledger=L` returns the same root the
      tree had immediately after the last leaf with `ledger <= L`
- [ ] Dropping the database and re-indexing reproduces an identical
      `tree_roots` table (replayability invariant)
- [ ] Disclosure `verify` (Issue 4) can validate a week-old report while
      deposits continue to land
- [ ] Answering a historical root query does not rebuild the tree (index
      lookup only)

---

## Issue 6 — Credential mailbox lifecycle: claim endpoint, idempotent pickup, retention

**Status: implemented.** Claim tokens per `docs/credential-mailbox.md`; `POST /v1/credentials/{id}/claim` (403/409 semantics verified end-to-end); pickup paginated by a new `seq` cursor; hourly retention sweeper with `CREDENTIAL_RETENTION_*` knobs (0 disables).

**Labels:** `backend`, `api`, `M5`

### Description

`credential_deliveries` has a `claimed_at` column and the pickup query
filters `claimed_at IS NULL` — but **no code ever sets `claimed_at`**, so
every delivery is returned on every pickup, forever, and the mailbox table
grows without bound. The lifecycle needs an explicit design:

- Recipients are anonymous (a `recipient_hint` is an opaque mailbox tag,
  deliberately not an identity), so "claiming" must not require an
  account — but it also must not let a third party who guesses a hint
  mark someone else's mail as claimed (denial of delivery).
- Undeliverable ciphertext (wrong hint, abandoned mailbox) should
  eventually be evicted.

A reasonable design: pickup stays idempotent and read-only; claiming
requires presenting a `claim_token` that was inside the encrypted payload
(only the true recipient can decrypt it), making malicious claiming
impossible without breaking the encryption.

### Tasks

- [ ] Design doc (short, in `docs/`): claim flow, why claim tokens beat
      hint-based claiming, retention windows
- [ ] Migration: add `claim_token_hash BYTEA` to `credential_deliveries`;
      issuers include the token hash at delivery time
- [ ] `POST /v1/credentials/{delivery_id}/claim` with the preimage; verify
      hash, set `claimed_at`, return 204 (409 if already claimed)
- [ ] Pagination for `GET /v1/credentials` (same cursor pattern as
      `/v1/notes`) — unbounded result sets today
- [ ] Retention job: delete claimed rows after N days and unclaimed rows
      after M days (configurable; document defaults in `.env.example`)
- [ ] Tests: claim happy path, wrong token, double claim, claimed rows
      excluded from pickup, retention deletes the right rows

### Acceptance criteria

- [ ] After a successful claim, the delivery no longer appears in pickup
      results
- [ ] A claim attempt without the correct token preimage cannot mark a
      delivery claimed (403), even knowing `delivery_id` and
      `recipient_hint`
- [ ] Pickup of a mailbox with 10k deliveries returns pages, not one
      unbounded response
- [ ] Retention config of `0` disables deletion (self-hosters may want to
      keep everything); defaults documented
- [ ] No new column or endpoint accepts plaintext credential content
      (invariant holds — the claim token is a hash preimage, not a secret
      about the credential)

---

## Issue 7 — Abuse protection on the write path: rate limits, quotas, and body hygiene

**Status: implemented.** Hand-rolled per-IP token buckets (separate read/write budgets), SSE per-IP/global connection slots released on drop, per-issuer hourly quota, CORS allowlist. All `RATE_LIMIT_*`/`CORS_ALLOWED_ORIGINS` env-tunable; 0 disables. 429s carry `Retry-After` + ApiError JSON shape.

**Labels:** `backend`, `api`, `hardening`

### Description

The API's only write endpoint is `POST /v1/issuer/credentials`, and its
only current protections are a 256 KiB global body limit and a 64 KiB
ciphertext bound. There is no rate limiting anywhere: a single client can
insert unlimited `credential_deliveries` rows (until Issue 3's signature
check lands, under any active issuer's name) and can hammer read
endpoints like `/v1/tree/{pool}/path` (which, while cheap now, still takes
the shared tree lock) and `/v1/notes/stream` (each connection holds a
broadcast receiver).

This is infrastructure that self-hosters expose publicly; it should ship
with sane limits by default rather than assuming a reverse proxy.

### Tasks

- [ ] Add per-IP token-bucket rate limiting via `tower` middleware
      (`tower_governor` or hand-rolled) with separate budgets for reads,
      writes, and SSE connection attempts
- [ ] Cap concurrent SSE connections per IP and globally (reject with 429
      + `Retry-After`)
- [ ] Per-issuer delivery quota (rows/hour) enforced in
      `deliver_credential`, configurable, so one compromised issuer key
      cannot flood the mailbox table
- [ ] Add `CorsLayer` with an explicit allowlist (the `cors` feature of
      tower-http is already a dependency but no layer is installed —
      browser provers will need it; document the config knob)
- [ ] Make limits configurable via env (`RATE_LIMIT_*`), with all limits
      disable-able for private deployments
- [ ] Tests: burst over the write budget → 429; reads unaffected by write
      throttling; quota resets after window

### Acceptance criteria

- [ ] Sustained write flooding from one IP is capped at the configured
      rate; the DB row count stops growing at the cap
- [ ] 429 responses include `Retry-After` and a JSON error body consistent
      with `ApiError`'s format
- [ ] SSE connection exhaustion from one client cannot starve other
      subscribers
- [ ] A browser client on an allowlisted origin can call every `/v1` read
      endpoint (CORS preflight passes)
- [ ] All limits off → behavior identical to today (self-host escape hatch)

---

## Issue 8 — SSE resume with Last-Event-ID and push-based note fan-out

**Status: implemented.** Events carry `id:`; `Last-Event-ID`/`?since_cursor=` replay is subscribe-then-fetch with cursor dedup (no gaps, no duplicates, verified); LISTEN/NOTIFY push with a polling safety net and fallback; `resync` events on lag or replay overflow.

**Labels:** `backend`, `api`, `enhancement`

### Description

`GET /v1/notes/stream` works but has two rough edges
(`crates/attesta-api/src/routes/notes.rs`):

1. **No resume.** Events carry no `id:` field, so the standard SSE
   `Last-Event-ID` reconnect header can't work. A client that drops for
   30 seconds silently misses notes and must know to re-sync via
   `/v1/notes` — easy to get wrong in wallet implementations.
2. **Polling fan-out.** The broadcaster polls the `encrypted_notes` table
   every 2 s. That's up to 2 s of added latency per note and constant
   idle queries. Postgres `LISTEN/NOTIFY` (a trigger on insert) gives
   push latency while keeping the API↔indexer decoupling (they still
   share only the database).

The note `id` column is already a monotonic cursor, which makes both
fixes natural.

### Tasks

- [ ] Set `id: <note.id>` on every SSE event
- [ ] Honor the `Last-Event-ID` request header (and a `since_cursor` query
      param equivalent): on connect, replay rows `> id` from the DB before
      switching to the live broadcast, without gaps or duplicates
      (fetch-then-subscribe with overlap dedup)
- [ ] Add a `NOTIFY attesta_notes` trigger in a new migration; API
      `LISTEN`s and falls back to the 2 s poll if the connection drops
      (poll stays as the safety net)
- [ ] Surface broadcast-lag drops to the client as a `resync` SSE event so
      well-behaved clients know to re-page instead of silently missing data
- [ ] Integration test: connect, receive live note, disconnect, insert 3
      notes, reconnect with `Last-Event-ID` → exactly those 3 arrive first

### Acceptance criteria

- [ ] A client reconnecting with `Last-Event-ID: N` receives every note
      with `id > N` exactly once, in order, then continues live
- [ ] End-to-end latency from row insert to SSE delivery is < 250 ms with
      LISTEN/NOTIFY active (measured in the integration test)
- [ ] Killing the LISTEN connection degrades to polling without dropping
      the stream (observable in logs; stream stays up)
- [ ] A slow consumer that overflows the broadcast buffer receives
      `resync` rather than nothing

---

## Issue 9 — Observability: Prometheus metrics, indexer lag, and a real readiness probe

**Status: implemented** (metrics + probes; see `docs/operations.md`). API `/metrics`, opt-in indexer listener via `INDEXER_METRICS_ADDR`, lag/undecodable/sync-error series, `/health/live` + `/health/ready` (DB check + optional indexer staleness), container healthchecks on readiness.

**Labels:** `backend`, `api`, `indexer`, `operations`

### Description

`/health` returns static JSON without touching the database, and neither
binary exports metrics. An operator today cannot answer: is the indexer
keeping up with the chain? are events failing to decode (Issue 2)? how hot
is the tree lock? did the note poller die? The indexer's ingest loop also
swallows errors with a `warn!` and retries forever — invisible unless
someone reads logs.

### Tasks

- [ ] Add a `/metrics` endpoint (metrics-rs + prometheus exporter) to the
      API, and a small metrics HTTP listener to the indexer (separate port,
      off by default)
- [ ] API metrics: request count/latency by route and status, SSE
      subscriber gauge, tree top-up duration, tree lock wait time,
      per-pool leaf count
- [ ] Indexer metrics: last ingested ledger vs. RPC latest ledger (lag
      gauge, per contract), events decoded/skipped/undecodable counters,
      sync error counter, loop duration histogram
- [ ] Split health into `/health/live` (process up) and `/health/ready`
      (DB reachable via `SELECT 1`, migrations current, and — optionally —
      indexer lag under a threshold read from `indexer_cursors`)
- [ ] Update docker-compose healthchecks and the Dockerfile HEALTHCHECK to
      use readiness
- [ ] Document scraping + a starter alert list (indexer lag, undecodable
      events > 0, 5xx rate) in `docs/operations.md`

### Acceptance criteria

- [ ] `curl /metrics` on the API exposes the listed series with data after
      one request and one SSE connection
- [ ] Stopping Postgres flips `/health/ready` to 503 while
      `/health/live` stays 200; restarting recovers without a process
      restart
- [ ] With the mock-RPC harness, halting the mock makes the indexer lag
      gauge grow; resuming brings it back to ~0
- [ ] A deliberately malformed event increments the undecodable counter
      (ties into Issue 2's acceptance)
- [ ] Metrics endpoints expose no secrets and no per-user data — pool ids,
      counts, and timings only (invariant check)

---

## Issue 10 — CI pipeline with the end-to-end integration harness

**Labels:** `backend`, `ci`, `testing`

### Description

The repo has no `.github/workflows/` — `cargo test`, clippy, and fmt run
only on developer machines, and the end-to-end path (indexer decoding XDR
from an RPC → Postgres → API tree/notes endpoints) is exercised only
manually via the recipe in `.claude/skills/verify/SKILL.md`. That recipe
already proved its worth (it caught the leaf-gap regression risk when it
was first run); it should run on every PR, not on demand.

### Tasks

- [ ] `ci.yml`: on PR and main push — `cargo fmt --check`,
      `cargo clippy --all-targets -- -D warnings`, `cargo test`, with
      dependency and build caching (`Swatinem/rust-cache`)
- [ ] Promote the mock Soroban RPC from the verify scratchpad into a
      checked-in test fixture (`tests/support/mock_rpc.rs` as a tokio
      task, or the Python server under `tests/support/`)
- [ ] Add an integration test crate/job: Postgres service container →
      run migrations → run indexer against mock RPC until cursors advance
      → assert all five tables → start API → assert tree root/path
      (with external path re-verification), notes pagination, and the
      leaf-gap guard
- [ ] Build the Docker image in CI (no push) so Dockerfile rot is caught
- [ ] Add a minimal MSRV/toolchain check honoring `rust-toolchain.toml`
- [ ] Badge in README; document how to run the integration suite locally
      (`cargo test -p attesta-integration -- --ignored` or similar)

### Acceptance criteria

- [ ] A PR that breaks fmt, clippy, any unit test, the integration flow,
      or the Docker build goes red
- [ ] The integration job reproduces the manual verification: XDR events
      ingested, tree served, externally recomputed path root matches,
      gap guard triggers on a seeded gap
- [ ] Total CI wall time under ~10 minutes with warm caches
- [ ] Integration suite runs locally with one command against
      `docker compose up -d db`, with no cloud dependencies
