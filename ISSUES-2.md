# Attesta Backend — Issue Backlog, Wave 2

Ten new issues grounded in the current code, following the first wave in
[ISSUES.md](ISSUES.md) (whose issues 5–9 are implemented and 1–4 remain
blocked on external deliverables). Each issue is written to be
copy-pasted into the tracker as-is. Labels follow CONTRIBUTING
conventions; the no-secrets-server invariant applies to every issue here.

Ordering is by suggested priority, not strictly by number.

---

## Issue 11 — Graceful shutdown and startup resilience

**Labels:** `backend`, `api`, `indexer`, `operations`

### Description

Neither binary handles its lifecycle deliberately:

- **Shutdown.** `axum::serve` in `crates/attesta-api/src/main.rs` runs
  without `with_graceful_shutdown`, so SIGTERM (every `docker stop`,
  every deploy) kills in-flight requests mid-response, drops SSE
  subscribers without a final event, and can interrupt the retention
  sweeper mid-batch. The indexer's ingest loop likewise dies wherever it
  happens to be — safe only because ingest is idempotent, but it still
  abandons a half-drained `getEvents` page it must re-fetch on restart.
- **Startup.** `db::connect` (`crates/attesta-core/src/db.rs`) makes one
  connection attempt. If Postgres is a second late (compose cold start,
  node reboot ordering), the process exits and stays dead unless a
  supervisor restarts it. `depends_on: service_healthy` covers compose,
  but bare-metal and k8s deployments hit this immediately.

### Tasks

- [ ] API: wire `with_graceful_shutdown` on SIGTERM/SIGINT — stop
      accepting, let in-flight requests finish (bounded, e.g. 15 s),
      close SSE streams cleanly
- [ ] Indexer: catch the same signals; finish the current `sync_contract`
      pass (or abort between pages), persist the cursor, then exit 0
- [ ] `db::connect`: bounded exponential-backoff retry (e.g. 10 attempts
      over ~60 s) with a clear log line per attempt; keep failing fast on
      *authentication* errors, which retrying cannot fix
- [ ] Stop background tasks (retention sweeper, note fan-out, metrics
      upkeep) via a shutdown token instead of relying on process death
- [ ] Add `stop_grace_period` to docker-compose services; document the
      shutdown sequence in `docs/operations.md`
- [ ] Tests: a SIGTERM during an in-flight `/v1/tree/{pool}/path` request
      returns the full response; the indexer exits with its cursor row
      matching the last fully processed page

### Acceptance criteria

- [ ] `docker stop` (default 10 s grace) produces zero client-visible
      connection resets under a light request load
- [ ] SIGTERM to the indexer never leaves `indexer_cursors` pointing past
      an unprocessed event or before a processed one (replay-safe today —
      must stay exactly as safe, just cleaner)
- [ ] With Postgres started 30 s *after* the API, the API comes up on its
      own with no supervisor restart
- [ ] A wrong `DATABASE_URL` password still fails fast with a clear error
      (no pointless 60 s retry loop)

---

## Issue 12 — Pool-scoped SSE streams

**Labels:** `backend`, `api`, `enhancement`

### Description

`GET /v1/notes` accepts a `pool` filter, but `GET /v1/notes/stream`
(`crates/attesta-api/src/routes/notes.rs`) does not: every subscriber
receives every pool's notes and must filter client-side. Today with one
pool that is harmless; with many pools it multiplies bandwidth per
subscriber, makes slow-consumer broadcast lag (and the resulting `resync`
events) more likely, and leaks cross-pool traffic *volume* patterns to
subscribers who only care about one pool. The `Last-Event-ID` replay
query in `stream_notes` also ignores pool, so a filtered stream must
filter the replay identically or reconnects would leak other pools'
rows.

### Tasks

- [ ] Add `?pool=` to `StreamQuery`; apply it to the replay SQL (same
      `($2::text IS NULL OR pool = $2)` pattern `list_notes` uses) and to
      the live broadcast filter closure
- [ ] Support a comma-separated multi-pool form (`?pool=A,B`) since
      wallets commonly watch a small set
- [ ] Keep cursor semantics global (ids are global) and document that a
      pool-filtered stream still uses the global cursor — switching
      filters across reconnects stays gap-free
- [ ] Update the README API reference and the verify skill's SSE flow
- [ ] Tests: filtered stream receives only matching pools in both replay
      and live phases; unfiltered behavior unchanged

### Acceptance criteria

- [ ] A subscriber on `?pool=A` never receives a note for pool B, whether
      it arrived via replay or live broadcast
- [ ] Reconnecting with `Last-Event-ID` on a filtered stream replays
      exactly the missed notes *for that filter*, in order, no duplicates
- [ ] An unfiltered stream behaves byte-for-byte as today

---

## Issue 13 — Indexer resilience: per-contract isolation, poison events, and RPC retention detection

**Labels:** `backend`, `indexer`, `operations`

### Description

`crates/attesta-indexer/src/ingest.rs` runs all contracts sequentially in
one loop, and its failure handling has three sharp edges:

1. **No isolation.** A slow or erroring RPC call for contract A delays
   contract B's sync by the full timeout, every cycle.
2. **Poison events.** A decodable event whose `store_event` INSERT fails
   permanently (e.g. a commitment whose bytes violate a future constraint,
   or a malformed issuer row) aborts `sync_contract` *before the cursor
   advances* — the loop re-fetches and re-fails the same page forever.
   The lag gauge grows, but ingest for that contract is permanently
   wedged with no operator guidance.
3. **RPC retention windows.** Soroban RPC only retains events for a
   bounded window (order of days). A cursor older than the window (long
   outage, restored old database) makes `getEvents` error forever. The
   indexer must detect this case explicitly, because the naive "resume
   where we left off" is impossible and silently retrying hides a
   *data-gap* condition that, for commitments, would corrupt tree
   indices if papered over.

### Tasks

- [ ] Spawn one task per configured contract with independent
      exponential backoff (cap ~60 s), replacing the sequential loop
- [ ] Distinguish storage failures from RPC failures in `sync_contract`;
      on a persistent per-event storage failure, record the raw event in
      a new `quarantined_events` table (pool, ledger, tx, raw payload,
      error), advance past it, and increment a
      `attesta_indexer_events_quarantined_total` counter — never wedge
- [ ] **Exception:** never quarantine a `new_commitment` — skipping a
      leaf would shift every later leaf index. For commitments, halt that
      contract's ingest, set a `attesta_indexer_halted{contract}` gauge,
      and log an unambiguous operator instruction
- [ ] Detect the out-of-retention RPC error, halt that contract with the
      same gauge, and document the recovery: drop-and-replay from ledger
      0 (guaranteed correct by design) — never auto-skip
- [ ] Document all three behaviors and their alerts in
      `docs/operations.md`
- [ ] Tests: poison non-commitment event quarantines and ingest
      continues; poison commitment halts; two contracts sync
      independently when one RPC endpoint stalls (mock)

### Acceptance criteria

- [ ] With contract A's RPC hanging 30 s per call, contract B's lag gauge
      stays ~0
- [ ] A permanently unstorable `note` event lands in
      `quarantined_events` exactly once, ingest proceeds past it, and the
      counter shows 1
- [ ] A permanently unstorable `new_commitment` halts only that contract,
      raises the halted gauge, and the tree endpoints keep serving the
      pre-gap tree (existing gap guard)
- [ ] An out-of-retention cursor produces a halted gauge and a log naming
      the drop-and-replay recovery, not an infinite silent retry

---

## Issue 14 — Multi-replica deployment semantics

**Labels:** `backend`, `api`, `operations`

### Description

Several API features live in per-process memory, which is correct for one
replica and quietly different for N:

- **Rate limits and SSE slots** (`crates/attesta-api/src/limits.rs`):
  per-IP buckets and connection caps are per-process, so N replicas
  multiply every effective limit by N (modulo load-balancer affinity).
- **Retention sweeper** (`crates/attesta-api/src/retention.rs`): every
  replica runs it. Deletes are idempotent so this is safe, but it
  produces duplicate work and interleaved lock contention hourly.
- **Tree caches**: each replica maintains its own in-memory tree and
  independently writes `tree_roots` (safe — `ON CONFLICT DO NOTHING` and
  deterministic values — but worth stating and testing as a guarantee).

Nothing here is broken; the issue is to make the semantics *chosen*
rather than accidental, and fix the one real gap (the sweeper).

### Tasks

- [ ] Retention sweeper: take a Postgres advisory lock
      (`pg_try_advisory_lock`) so exactly one replica sweeps per cycle;
      the others skip with a debug log
- [ ] Document replica semantics in `docs/operations.md`: rate limits are
      per-replica (state the N× effect and recommend LB-level limits for
      strict global caps), SSE caps are per-replica, tree caches converge
      independently
- [ ] Add a test asserting two concurrent `sync_pool_tree` writers (two
      pools maps, same DB) produce a single consistent `tree_roots`
      history — codify the existing guarantee
- [ ] Optional (design-first, separate PR): a `SharedLimiter` trait so a
      Redis/Postgres-backed global limiter can slot in later without
      touching the middleware
- [ ] Compose example for 2 API replicas behind a proxy (documentation,
      profile `scale`)

### Acceptance criteria

- [ ] With 3 API replicas against one database, exactly one performs
      retention deletes per hour (observable via the sweep log line and
      `pg_locks`)
- [ ] Two replicas serving the same pool concurrently produce identical
      `tree_roots` rows and identical roots for the same leaf count
- [ ] `docs/operations.md` states, per feature, whether it is
      per-replica or global — no undocumented sharing assumptions

---

## Issue 15 — Consistency self-audit: gaps, root history, and a repair-signal metric

**Labels:** `backend`, `api`, `indexer`, `operations`

### Description

The tree gap guard in `sync_pool_tree`
(`crates/attesta-api/src/routes/tree.rs`) protects correctness but only
*logs* at request time — an operator learns about a leaf-index gap when a
prover complains. There is no proactive check that:

- `commitments.leaf_index` is dense (0..n-1) per pool,
- persisted `tree_roots` match roots recomputed from the leaves,
- `pool_totals` reconcile with the sum of decoded deposit/withdrawal
  events (drift here would be an ingest bug, not chain state).

All state is replayable, so any detected corruption has a
guaranteed-correct fix (drop and re-index) — the missing piece is
*detection you don't have to wait for*.

### Tasks

- [ ] New `audit` module in `attesta-core` with pure checks:
      `find_leaf_gaps(pool)`, `verify_root_history(pool)` (recompute the
      incremental tree over all leaves, compare each `tree_roots` row),
      `totals_are_nonnegative()`
- [ ] Expose as a CLI: `cargo run --bin indexer -- --audit` (or a small
      `admin` binary) printing a machine-readable JSON verdict, exit 1 on
      any finding
- [ ] Background: API publishes `attesta_api_tree_gap{pool}` gauge (0/1)
      set by the existing gap-guard path, so the alert fires when the
      guard first trips instead of when a human reads logs
- [ ] Periodic light audit (leaf density only — cheap `count(*) vs
      max(leaf_index)+1`) in the indexer, gauge per pool
- [ ] Document the audit + the drop-and-replay repair in
      `docs/operations.md` with the alert rule
- [ ] Tests: seeded gap → gap finding + gauge; tampered `tree_roots` row
      → root-history finding; clean database → exit 0

### Acceptance criteria

- [ ] Deleting one middle commitment row makes the audit CLI exit 1
      naming the pool and missing index, and flips the gap gauge within
      one indexer poll cycle
- [ ] Corrupting one `tree_roots.root` byte is caught by
      `verify_root_history` with the offending leaf_count
- [ ] A clean, freshly replayed database audits green end-to-end
- [ ] The full root-history audit of a 100k-leaf pool completes in
      seconds (single pass, O(n·depth)) and is safe to run against a
      live database (read-only)

---

## Issue 16 — Artifacts CDN hardening: streaming, cached digests, and conditional requests

**Labels:** `backend`, `api`, `performance`

### Description

`crates/attesta-api/src/routes/artifacts.rs` reads the entire artifact
into memory (`tokio::fs::read`) and recomputes its SHA-256 **on every
request**. Proving keys are routinely hundreds of megabytes: a handful of
concurrent downloads can exhaust memory, and the per-request hash burns
CPU for a value that never changes (the layout is already
versioned-immutable — responses even carry `Cache-Control: immutable`).
There is also no `ETag`/`If-None-Match`, `Content-Length`, or `Range`
support, all of which matter for large files on flaky connections.

### Tasks

- [ ] Stream file bytes (`tokio_util::io::ReaderStream`) instead of
      buffering; set `Content-Length` from file metadata
- [ ] Load and verify each version's `manifest.json` **once** (startup or
      first request, cached in `AppState`); serve `x-artifact-sha256`
      from the manifest instead of recomputing — and *validate* on cache
      fill that the manifest's hash matches the file on disk, refusing to
      serve a mismatch (500 + error log: this is the integrity product)
- [ ] `ETag: "<sha256>"` + `If-None-Match` → 304
- [ ] `Accept-Ranges: bytes` with single-range support (resume for
      large proving keys)
- [ ] Reject manifests referencing files outside their version directory
      (defense-in-depth on top of the existing segment validation)
- [ ] Tests: 304 on matching ETag; range request returns the correct
      slice; tampered file on disk (hash mismatch vs manifest) → 500,
      never bytes-with-wrong-hash

### Acceptance criteria

- [ ] Downloading a 500 MB artifact keeps API RSS growth under ~10 MB
      (streamed, not buffered)
- [ ] Second and later downloads of the same artifact perform zero
      hashing (verified by timing or a counter metric)
- [ ] A client resuming at byte offset N receives exactly the remaining
      bytes and can reassemble to the manifest hash
- [ ] A tampered artifact is never served: hash mismatch turns into a
      500 with a loud log, not a silent wrong-hash response

---

## Issue 17 — Stats: bounded-cost queries, short-TTL cache, and per-pool detail

**Labels:** `backend`, `api`, `performance`

### Description

`GET /v1/stats` (`crates/attesta-api/src/routes/stats.rs`) issues four
unbounded `COUNT(*)` scans per request. Postgres makes these full-index
scans, so cost grows linearly with table size — on a busy pool with
millions of notes, the public stats endpoint becomes the most expensive
query in the system, invoked by anyone, repeatedly (it is rate-limited
per IP, but every distinct visitor pays the scan). There is also no
per-pool breakdown beyond TVL: no leaf counts, nullifier counts, or
recent-activity signal, which explorers and dashboards want.

### Tasks

- [ ] Cache the assembled `ProtocolStats` in `AppState` with a short TTL
      (e.g. 10 s, configurable) — one scan set per window regardless of
      traffic; stale-while-revalidate is fine
- [ ] Replace global `COUNT(*)` with per-pool grouped counts in one query
      (`SELECT pool, count(*) FROM commitments GROUP BY pool`), summing
      for totals; extend `PoolStats` with `commitments`, `nullifiers`,
      `notes` counts
- [ ] Add `GET /v1/pools/{pool}/stats` returning the single-pool view
      plus `leaf_count`/`anchored_ledger` from the tree cache (already in
      memory, free)
- [ ] Add `Cache-Control: public, max-age=10` on both endpoints so
      proxies/CDNs absorb repeat traffic
- [ ] Tests: cache serves within TTL without re-querying (query counter
      or timing), refreshes after; per-pool numbers match seeded rows

### Acceptance criteria

- [ ] 1000 sequential `/v1/stats` requests execute the underlying scans
      at most `⌈duration/TTL⌉` times (observable via Postgres
      `pg_stat_statements` or a metrics counter)
- [ ] Response shape stays backward-compatible (existing fields
      unchanged; new fields additive)
- [ ] `GET /v1/pools/{pool}/stats` for an unknown pool → 404 consistent
      with the tree endpoints
- [ ] No stats value can reveal a shielded amount (per-pool counts and
      public totals only — invariant check in review)

---

## Issue 18 — Request IDs and structured log output

**Labels:** `backend`, `api`, `operations`

### Description

The API installs `TraceLayer` with defaults: no request id is accepted,
generated, or returned, so a user-reported failure ("my path request
500'd at 14:02") cannot be correlated to a specific log line, metric
sample, or database error — `ApiError`'s from-sqlx conversion
deliberately hides details from clients (`crates/attesta-api/src/error.rs`),
which is correct, but the operator-side half of that trade (find the
hidden detail by id) does not exist yet. Log output is also plain-text
only, which log aggregators tolerate but parse badly.

### Tasks

- [ ] Add `tower_http::request_id`: honor an incoming `x-request-id`
      (sanitized, bounded length) or generate a UUID; echo it on every
      response including errors and 429s
- [ ] Put the request id into the `TraceLayer` span so every log line
      inside a request carries it
- [ ] Include the id in the JSON body of 5xx responses
      (`{"error": "internal error", "request_id": "…"}`) — safe to show,
      lets users report actionable failures
- [ ] `LOG_FORMAT=json|text` env knob (default `text`) switching
      `tracing_subscriber` to JSON output, both binaries
- [ ] Document the correlation workflow (id → log line → metric) in
      `docs/operations.md`
- [ ] Tests: response carries the echoed id; a 500's body id matches the
      logged span id

### Acceptance criteria

- [ ] Every response (2xx through 5xx) carries `x-request-id`; supplying
      one reuses it, omitting one generates it
- [ ] Grepping the log for a 500's `request_id` finds the underlying
      database error in one step
- [ ] `LOG_FORMAT=json` emits one valid JSON object per line, including
      the request id field; default output is unchanged
- [ ] No request id path logs request *bodies* or query strings
      containing mailbox hints (invariant: correlation ids, not payloads)

---

## Issue 19 — Durability story for `credential_deliveries`

**Labels:** `backend`, `operations`, `documentation`

### Description

Every table except `credential_deliveries` is replayable from chain
events — the README's drop-the-database recovery applies to all of them.
`credential_deliveries` is the sole exception: it exists nowhere else,
and losing it silently destroys undelivered credentials (recipients
cannot re-request what they never learned existed; issuers have no
delivery receipts to replay). Right now nothing in the repo acknowledges
this asymmetry: no backup guidance, no dump/restore tooling, no
interaction analysis with the retention sweeper (a restore of an old dump
could resurrect rows the sweeper already deleted — acceptable, but should
be stated).

### Tasks

- [ ] Document the asymmetry prominently: README data-model section and
      `docs/operations.md` ("everything is replayable *except* this
      table — treat it as the one thing you back up")
- [ ] Ship a minimal backup/restore recipe:
      `pg_dump --table=credential_deliveries` cron example and a restore
      procedure that preserves `seq` ordering (identity column —
      `--disable-triggers` note, sequence advancement)
- [ ] Make restores safe by construction: restore-then-replay must not
      collide with new deliveries (delivery_id is a UUID PK — verify and
      test; `seq` must be re-assigned or preserved consistently, decide
      and document)
- [ ] State the sweeper interaction: restored rows older than the
      retention windows are deleted on the next sweep; operators who
      want to keep them must set retention to 0 *before* restoring
- [ ] Optional: a `attesta_api_deliveries_unclaimed` gauge so operators
      can alert on an anomalous drop (a proxy for accidental data loss)
- [ ] Test: dump → truncate → restore → pickup returns identical pages,
      claims still verify against restored `claim_token_hash`

### Acceptance criteria

- [ ] A dump/restore round-trip preserves pickup pagination order and
      claim-token verification for every row
- [ ] Docs answer, without ambiguity: "which tables must I back up?"
      (exactly one) and "what happens if I restore an old dump?"
- [ ] New deliveries inserted after a restore never collide with
      restored rows (UUID + identity sequence behavior verified by test)
- [ ] The unclaimed-deliveries gauge (if implemented) appears in the
      starter alerts list

---

## Issue 20 — CI pipeline with the end-to-end integration harness

**Labels:** `backend`, `ci`, `testing`
**Supersedes:** ISSUES.md Issue 10 (unimplemented; scope extended to the
wave-1 features that have since landed)

### Description

The repo still has no `.github/workflows/` — fmt, clippy, and the 23 unit
tests run only on developer machines, and the full verified surface
(indexer XDR ingest → Postgres → tree/root-history endpoints, claim
lifecycle, SSE resume, rate limiting, readiness probes — all currently
exercised manually via `.claude/skills/verify/SKILL.md`) regresses
silently the first time someone skips the manual recipe. Everything the
manual recipe drives is automatable with a Postgres service container and
the mock Soroban RPC.

### Tasks

- [ ] `ci.yml` on PR + main push: `cargo fmt --check`,
      `cargo clippy --all-targets -- -D warnings`, `cargo test`, with
      `Swatinem/rust-cache`
- [ ] Promote the mock Soroban RPC into a checked-in fixture
      (`tests/support/`), serving the hand-encoded XDR events the verify
      skill documents
- [ ] Integration job (Postgres service container): run migrations →
      indexer against mock RPC until cursors advance → assert all tables
      → start API → assert: root + externally recomputed path,
      `at_ledger`/`at_leaf_count` historical roots, leaf-gap guard,
      claim lifecycle (403/204/409), pickup pagination, SSE
      `Last-Event-ID` replay + live NOTIFY delivery, write-burst 429
      with `Retry-After`, `/health/ready` 503 on DB stop, `/metrics`
      series presence
- [ ] Build the Docker image in CI (no push) so Dockerfile rot is caught
- [ ] Toolchain check honoring `rust-toolchain.toml`; README badge;
      document the one-command local run
      (`cargo test -p attesta-integration -- --ignored` against
      `docker compose up -d db`)

### Acceptance criteria

- [ ] A PR breaking fmt, clippy, any unit test, any integration
      assertion above, or the Docker build goes red
- [ ] The integration job reproduces the manual verify recipe end to end
      with zero cloud dependencies
- [ ] Total CI wall time under ~10 minutes with warm caches
- [ ] The verify skill doc points to the integration suite as the
      automated equivalent, keeping the manual recipe for exploratory
      debugging
