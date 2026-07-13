# Attesta Protocol — Backend

Backend services for **Attesta**, a confidential payments layer with built-in
compliance for the Stellar ecosystem: shielded transfer amounts with selective
disclosure for auditors, plus reusable ZK compliance attestations. Built on
Stellar Protocol 25's zero-knowledge primitives (BLS12-381 on Soroban).

**Stack:** Rust (Axum) · PostgreSQL · Soroban RPC
**License:** Apache-2.0

- [What Attesta does](#what-attesta-does)
- [The trust rule](#the-trust-rule)
- [Architecture](#architecture)
- [How it works, end to end](#how-it-works-end-to-end)
- [The components in detail](#the-components-in-detail)
- [Data model](#data-model)
- [API reference](#api-reference)
- [Development setup](#development-setup)
- [Design decisions](#design-decisions)
- [Status / TODO toward M2](#status--todo-toward-m2-shielded-pool-mvp)
- [Contributing](#contributing)

## What Attesta does

On a public ledger, every payment amount is visible to everyone. That is a
non-starter for payroll, B2B settlement, and most real financial activity —
but so is opacity that regulators and auditors cannot pierce when they have
a legitimate mandate. Attesta resolves the tension with two mechanisms:

1. **Shielded pools.** Users deposit assets into a Soroban smart contract
   (a public, amount-visible action) and receive *notes* — private records
   of value, represented on-chain only by cryptographic **commitments** in
   a Merkle tree. Transfers inside the pool prove, in zero knowledge, that
   the sender owns an unspent note and that value is conserved, without
   revealing amounts, sender, or recipient. Withdrawals exit the pool with
   a public amount. So the pool's *total* value (TVL) is public by
   construction, while everything inside it is private.

2. **Selective disclosure.** Privacy is not anonymity from everyone. Each
   account can derive **scoped viewing keys** that decrypt exactly the
   subset of activity they cover — nothing more. Handing an auditor a
   viewing key (out of band) lets them independently reconstruct and
   *cryptographically verify* the covered payment history against on-chain
   commitments, with zero cooperation from this backend. Alongside this,
   registered **issuers** (KYC providers, accreditation services) deliver
   encrypted, reusable compliance credentials that users can later present
   inside ZK proofs ("this transfer's sender holds a valid KYC credential")
   without revealing who they are.

This repository is the **backend** for that system: the indexing,
relaying, and serving infrastructure that makes clients fast and
self-hosting practical. It is deliberately *not* trusted with anything.

## The trust rule

Proofs are generated **client-side**, in the browser or CLI. Private amounts,
credentials, spending keys, and viewing keys never leave the user's device.
This backend is infrastructure for convenience and discovery — it relays
ciphertext and indexes public chain state, and is **architecturally incapable
of learning secrets**. A fully compromised backend can censor convenience
(refuse to serve data, serve stale data), but can never learn an amount,
identify a note's owner, or forge a proof — clients verify everything they
receive against on-chain state.

> **Standing invariant (hard, enforced in review):** no change may create a
> code path where a plaintext amount, spending key, or raw credential reaches
> this backend. There is no endpoint that accepts one, and no column that
> stores one.

## Architecture

```
        CLIENTS (browser / CLI — proving, keys, decryption)
                            │ REST / SSE (never secrets)
┌───────────────────────────▼──────────────────────────────────┐
│                     BACKEND (this repo)                       │
│                                                               │
│   attesta-api ◄──────── PostgreSQL ────────► attesta-indexer  │
│   (REST + SSE)        (shared state,          (event ingest)  │
│                     public + ciphertext)            │         │
│                                                     │         │
│   attesta-disclosure (local CLI, runs on the        │         │
│   auditor's machine — not a server)                 │         │
└─────────────────────────────────────────────────────┼─────────┘
                                                      │ Soroban RPC
                                                      │ (getEvents)
                 STELLAR (shielded pool + registry contracts)
```

| Path | Binary | Role |
| --- | --- | --- |
| `crates/attesta-api` | `api` | REST + SSE API: Merkle paths, note relay, issuer gateway, stats, prover-artifacts CDN |
| `crates/attesta-indexer` | `indexer` | Mirrors the on-chain commitment tree, nullifier set, encrypted notes, and issuer registry from Soroban events |
| `crates/attesta-disclosure` | `disclosure` | Local CLI: builds auditor disclosure packages from a scoped viewing key (the key never leaves the machine) |
| `crates/attesta-core` | — | Shared config, DB layer, models, incremental Merkle tree |
| `migrations/` | — | PostgreSQL schema (public chain state + ciphertext only) |
| `artifacts/` | — | Versioned proving keys / WASM provers served with published hashes |

The API and indexer are separate processes that share only the database.
Either can restart, crash, or be scaled independently; neither calls the
other.

## How it works, end to end

### Deposit

1. A user calls the pool contract with a public amount and a fresh
   **commitment** `C = H(amount, owner, randomness)` computed client-side.
2. The contract appends `C` to its on-chain incremental Merkle tree and
   emits a `new_commitment` event carrying the commitment, its
   **leaf index**, and the public deposit amount.
3. The indexer picks the event up from Soroban RPC, stores the leaf in
   `commitments`, and adds the amount to `pool_totals` (this is why TVL is
   public: value only crosses the shielded boundary with visible amounts).

### Shielded transfer

1. The sender's wallet fetches a **Merkle path** for its note's commitment
   from `GET /v1/tree/{pool}/path` and the current root, then builds a ZK
   proof client-side: *"I know a note in the tree with this root, I own
   it, and the new output commitments conserve its value."* The proving
   key and WASM prover come from the artifacts CDN and are
   integrity-checked against published SHA-256 hashes before use.
2. The proof publicly reveals only: the tree root it anchored to, a
   **nullifier** (a one-way tag derived from the spent note that prevents
   double-spends without identifying which leaf was spent), and the new
   output commitments.
3. The contract verifies the proof on-chain (BLS12-381 pairing check),
   records the nullifier, appends the outputs, and emits events — among
   them an **encrypted note**: the output note's contents encrypted to the
   recipient's key, published as `(ephemeral_pubkey, ciphertext)`.
4. The indexer mirrors all of it: `nullifiers`, new `commitments`, and
   `encrypted_notes` rows.

### Receiving

Recipients don't get told they were paid — they *discover* it. A wallet
pages `GET /v1/notes` (or holds the `GET /v1/notes/stream` SSE stream) and
**trial-decrypts** every ciphertext with its viewing key. Almost all fail
(they're for other people); the ones that succeed are incoming payments.
The relay cannot tell which notes belong to whom — it serves the same
ciphertext firehose to everyone. This is the discovery convenience the
backend exists to provide, and it learns nothing while providing it.

### Withdrawal

A withdrawal proof reveals a nullifier and a public exit amount; the
contract pays out and emits a `withdrawal` event, which the indexer
subtracts from `pool_totals`.

### Compliance credentials

1. Issuers register in the on-chain registry (name, Ed25519 public key,
   claim types); the indexer mirrors it into `issuers`, served at
   `GET /v1/issuers`.
2. After off-chain verification (KYC etc.), the issuer encrypts a
   credential *to the recipient* and POSTs the ciphertext to the
   **issuer gateway** (`POST /v1/issuer/credentials`) along with an opaque
   `recipient_hint` — a mailbox tag the recipient derived, not an
   identity.
3. The recipient polls `GET /v1/credentials?recipient_hint=…` and decrypts
   locally. Later, they can prove facts *about* the credential inside a ZK
   proof without showing the credential itself.

The gateway is a dead drop: it sees issuer id, mailbox tag, ciphertext,
signature. There is deliberately no field in the request schema where
plaintext claim data could go.

### Auditor disclosure

1. An account holder derives a **scoped viewing key** and hands it to the
   auditor out of band. The key's scope bounds what it can ever decrypt.
2. The auditor runs the local `disclosure` CLI: it fetches *public* data
   (all encrypted notes, tree root) from any backend — including one the
   auditor self-hosts, if they don't trust anyone — and trial-decrypts
   locally. The key never leaves the machine.
3. The output report contains the decrypted, in-scope entries plus each
   entry's Merkle path, so `disclosure verify` can later re-check every
   claim against on-chain commitments independently.

### The Merkle tree, concretely

The chain is the source of truth for the tree; the backend maintains a
faithful mirror so provers don't need to replay the chain themselves.
`attesta-core`'s tree is a depth-32 append-only incremental Merkle tree
with every internal level cached: appends cost O(depth), the root is O(1),
and a path is O(depth). The API keeps one long-lived tree per pool in
memory and, on each request, tops it up with any newly indexed leaves from
the `commitments` table — no per-request rebuilds. If the indexer ever
leaves a gap in leaf indexes (mid-backfill or a missed event), the tree
**stops before the gap** rather than appending leaves at wrong positions:
a misplaced leaf would make every client proof silently unverifiable,
which is strictly worse than serving a shorter, correct tree.

The hash is currently a SHA-256 placeholder behind the `TreeHasher` trait;
production requires the same Poseidon-over-BLS12-381 instance the circuits
use (see [ISSUES.md](ISSUES.md), Issue 1 — security-critical).

## The components in detail

### `attesta-api`

Axum HTTP server (`:8080` by default). Runs migrations on startup. Serves:

- **Tree endpoints** — per-pool cached incremental tree (see above),
  returning paths, roots, leaf counts, and the `anchored_ledger` (the
  ledger of the newest indexed leaf, which clients pin proofs to).
- **Note relay** — cursor-paginated ciphertext pages (`id` is the cursor,
  500 rows/page) and an SSE stream. A background poller watches
  `encrypted_notes` every 2 s and fans new rows out to SSE subscribers
  over a broadcast channel; slow subscribers that miss broadcasts re-sync
  via the paginated endpoint.
- **Issuer gateway** — credential dead-drop with size caps (64 KiB
  ciphertext) and active-issuer checks. Issuer signature verification is
  pending the M5 envelope format (Issue 3).
- **Stats** — public-by-construction numbers: per-pool TVL
  (`total_in − total_out`), commitment/nullifier counts, issuer and
  delivery counts.
- **Artifacts CDN** — versioned proving keys and WASM provers from
  `ARTIFACTS_DIR`, each served with an `x-artifact-sha256` header and an
  immutable cache policy; a `manifest.json` per circuit version lists
  files and hashes. Path segments are strictly validated (no traversal).

A 256 KiB global request-body limit is enforced; ciphertext blobs are
small by design.

### `attesta-indexer`

A poll loop per configured contract (`POOL_CONTRACT_IDS`, plus optionally
`REGISTRY_CONTRACT_ID`). Each pass:

1. Loads the contract's cursor from `indexer_cursors` (last ledger + RPC
   pagination cursor).
2. Calls Soroban RPC `getEvents`, pages until drained.
3. Decodes each event from base64-XDR `ScVal`s
   (`crates/attesta-indexer/src/events.rs`): the first topic is the event
   name symbol; the payload is a symbol-keyed map. Recognized events:
   `new_commitment`, `nullifier` (map or bare bytes), `note`,
   `withdrawal`, `issuer`. The layout is provisional until the contracts
   freeze (Issue 2); unknown events are skipped with a debug log,
   recognized-but-undecodable ones warn.
4. Stores typed events with `ON CONFLICT DO NOTHING` on natural unique
   keys — ingest is **idempotent**, so replays, cursor resets, and
   crash-restarts are always safe, and the entire database can be rebuilt
   from ledger 0 at any time.

### `attesta-disclosure`

A local CLI, not a service. `generate` reads a viewing key file, fetches
the pool root and every encrypted note from a configurable backend
(`ATTESTA_API_URL`), and writes a JSON report; `verify` re-checks a report
against a backend. Trial decryption and per-entry Merkle verification are
pending the M3 note format (Issue 4) — today the report carries the scan
scope and root anchor with an empty entries list.

### `attesta-core`

Shared library: env config (`Config::from_env`), sqlx Postgres pool +
embedded migrations, serde row models (byte columns serialize as 0x-hex),
and the incremental Merkle tree with its `TreeHasher` abstraction.

## Data model

All tables are either public chain state or ciphertext — restated per the
invariant, there is no column for a plaintext amount, key, or credential.

| Table | Contents | Written by |
| --- | --- | --- |
| `indexer_cursors` | per-contract ingest progress (ledger + RPC cursor) | indexer |
| `commitments` | tree leaves: `(pool, leaf_index, commitment, ledger, tx_hash)`, unique per pool on both leaf index and commitment | indexer |
| `nullifiers` | spent-note tags, unique per pool | indexer |
| `encrypted_notes` | `(commitment, ephemeral_pubkey, ciphertext)` blobs; serial `id` doubles as the pagination cursor | indexer |
| `issuers` | registry mirror: key, claim types, status (`active`/`suspended`/`revoked`) | indexer |
| `credential_deliveries` | issuer-gateway dead drops: ciphertext + signature keyed by opaque `recipient_hint` | API |
| `pool_totals` | running public in/out totals per pool (TVL = in − out) | indexer |

Everything the indexer writes is replayable from chain events;
`credential_deliveries` is the only state that exists nowhere else, and it
is ciphertext a recipient re-fetches at leisure.

## API reference

```
GET  /health                               → liveness
GET  /v1/tree/{pool}/path?commitment=0x…   → Merkle path for proving
       { pool, leaf_index, root, leaf_count, anchored_ledger,
         path: [{ sibling, sibling_on_right }] × 32 }
GET  /v1/tree/{pool}/root                  → { pool, root, leaf_count, anchored_ledger }
       ?at_ledger=L      → newest root anchored at or before ledger L
       ?at_leaf_count=N  → root the tree had at leaf count ≤ N
       (historical answers come from the tree_roots table — an index
        lookup, never a rebuild; history replays identically after a
        database drop)
GET  /v1/notes?since_cursor=&pool=         → { notes: […], next_cursor } (500/page)
GET  /v1/notes/stream                      → SSE, event: note, 20 s keep-alive
POST /v1/issuer/credentials                → { issuer_id, recipient_hint,
                                               ciphertext (b64), issuer_signature (b64),
                                               claim_token_hash (b64, optional) }
                                             → 201 { delivery_id }
GET  /v1/credentials?recipient_hint=&since_cursor=
                                           → { deliveries: […], next_cursor } (200/page,
                                             unclaimed only; idempotent, read-only)
POST /v1/credentials/{delivery_id}/claim   → { claim_token (b64) } → 204
                                             (403 wrong token, 409 already claimed;
                                              see docs/credential-mailbox.md)
GET  /v1/issuers                           → non-revoked issuer registry mirror
GET  /v1/stats                             → { pools: [{pool, asset, tvl}], counts… }
GET  /v1/artifacts/{circuit}/{version}     → manifest.json (file list + sha256)
GET  /v1/artifacts/{circuit}/{version}/{f} → artifact bytes + x-artifact-sha256 header
```

Errors are JSON with appropriate status codes: 400 for malformed input
(bad hex, wrong lengths, oversized bodies), 404 for unknown
pools/commitments/artifacts, 403/409 for claim failures, and 429 (with
`Retry-After`) when a rate limit or quota trips.

Abuse protection ships on by default (per-IP token buckets with separate
read/write budgets, SSE connection caps, a per-issuer hourly delivery
quota, and an optional CORS allowlist for browser provers). Every limit
is a `RATE_LIMIT_*` / `CORS_ALLOWED_ORIGINS` env knob and `0` disables
it — see `.env.example`. Limits key on the socket peer address, so when
running behind a reverse proxy, enforce limits there as well.

## Development setup

Prerequisites: Rust (pinned by `rust-toolchain.toml`), Docker.

```bash
cp .env.example .env
docker compose up -d db          # Postgres 16
cargo run --bin api              # serves on :8080, runs migrations
cargo run --bin indexer          # idles until POOL_CONTRACT_IDS is set
```

Or the full self-hosted stack in containers:

```bash
docker compose --profile full up --build
```

Run tests and lints:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

For end-to-end verification (driving the indexer against a mock Soroban RPC
and exercising the tree endpoints), see `.claude/skills/verify/SKILL.md`.

### Configuration

All via environment (see `.env.example`): `DATABASE_URL`, `BIND_ADDR`,
`SOROBAN_RPC_URL`, `POOL_CONTRACT_IDS` (comma-separated),
`REGISTRY_CONTRACT_ID`, `INDEXER_POLL_SECS`, `ARTIFACTS_DIR`, `RUST_LOG`.
No secrets are needed to run any component — by design there are none to
configure.

### Disclosure CLI

```bash
# Generate an auditor report scoped to one viewing key (key stays local)
cargo run --bin disclosure -- generate \
    --pool <POOL_CONTRACT_ID> --viewing-key ./my.viewkey --output report.json

# Independently re-verify a report against any backend
cargo run --bin disclosure -- verify report.json
```

## Design decisions

- **No secrets, provably.** Request schemas only have room for ciphertext,
  commitments, and public identifiers. Enforced as a hard invariant in code
  review, not a guideline.
- **Replayable.** All indexer state rebuilds from chain events alone —
  dropping the database and restarting from ledger 0 is always safe
  (idempotent inserts on unique keys).
- **Self-hostable everything.** One image ships all three binaries; a company
  running payroll can run its own indexer, relay, and disclosure tooling
  end-to-end, and an auditor who trusts no one can point the disclosure CLI
  at their own stack.
- **Decoupled processes.** API and indexer share only the database; either
  can restart independently.
- **Integrity-checkable artifacts.** Proving keys and WASM provers are served
  with a manifest of SHA-256 hashes and an `x-artifact-sha256` response
  header, so clients verify before proving.
- **Cheap, honest tree serving.** The API keeps one incremental Merkle tree
  per pool in memory (O(depth) appends, O(1) root) and tops it up from the
  `commitments` table per request. If the indexer leaves a gap in leaf
  indexes, the tree stops before the gap rather than serving misplaced
  leaves — a wrong path would make client proofs silently unverifiable.

## Status / TODO toward M2 (shielded pool MVP)

- [x] Workspace, schema, migrations, docker compose, container image
- [x] Tree endpoints, note relay + SSE, issuer gateway, stats, artifacts CDN
- [x] Indexer loop with cursor persistence and idempotent, replayable ingest
- [x] Real XDR (`ScVal`) event decoding via `stellar-xdr`
      (`crates/attesta-indexer/src/events.rs` — decodes a provisional
      symbol-keyed map layout documented in the module; re-check the field
      tables once the pool contract's event layout freezes. The JSON shim
      remains as a test-only fallback)
- [x] Incremental Merkle tree with cached levels — O(depth) appends, O(1)
      root, O(depth) paths — plus a per-pool in-memory tree cache in the API
      that tops up from the `commitments` table instead of rebuilding per
      request
- [ ] Poseidon-over-BLS12-381 tree hasher matching the circuits
      (`crates/attesta-core/src/merkle.rs` — currently a SHA-256 placeholder
      behind the `TreeHasher` trait; **security-critical**, dual review)
- [ ] Issuer signature verification on credential delivery (blocked on the
      M5 credential envelope format)
- [ ] Disclosure trial-decryption + per-entry Merkle path verification
      (blocked on the M3 note encryption format)

The full backlog with per-issue tasks and acceptance criteria lives in
[ISSUES.md](ISSUES.md).

## Contributing

Issues are labeled `backend/good-first-issue` and by difficulty. Anything
touching the no-secrets-server invariant is `security-critical` and requires
dual review. See the main project README for the full contribution guide and
roadmap (M1–M7).

## License

Apache-2.0
