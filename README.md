# Attesta Protocol — Backend

Backend services for **Attesta**, a confidential payments layer with built-in
compliance for the Stellar ecosystem: shielded transfer amounts with selective
disclosure for auditors, plus reusable ZK compliance attestations. Built on
Stellar Protocol 25's zero-knowledge primitives (BLS12-381 on Soroban).

**Stack:** Rust (Axum) · PostgreSQL · Soroban RPC
**License:** Apache-2.0

## The trust rule

Proofs are generated **client-side**, in the browser or CLI. Private amounts,
credentials, and viewing keys never leave the user's device. This backend is
infrastructure for convenience and discovery — it relays ciphertext and
indexes public chain state, and is **architecturally incapable of learning
secrets**. A fully compromised backend can censor convenience, but can never
learn an amount or forge a proof.

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
│  Encrypted-note relay · Merkle tree indexer · Issuer gateway  │
│  Disclosure tooling · Public stats — HOLDS NO SECRETS         │
└───────────────────────────┬──────────────────────────────────┘
                            │ Soroban RPC (getEvents)
                 STELLAR (shielded pool + registry contracts)
```

## Repository layout

| Path | Binary | Role |
| --- | --- | --- |
| `crates/attesta-api` | `api` | REST + SSE API: Merkle paths, note relay, issuer gateway, stats, prover-artifacts CDN |
| `crates/attesta-indexer` | `indexer` | Mirrors the on-chain commitment tree, nullifier set, encrypted notes, and issuer registry from Soroban events |
| `crates/attesta-disclosure` | `disclosure` | Local CLI: builds auditor disclosure packages from a scoped viewing key (the key never leaves the machine) |
| `crates/attesta-core` | — | Shared config, DB layer, models, incremental Merkle tree |
| `migrations/` | — | PostgreSQL schema (public chain state + ciphertext only) |
| `artifacts/` | — | Versioned proving keys / WASM provers served with published hashes |

## API

```
GET  /health
GET  /v1/tree/{pool}/path?commitment=0x…   → Merkle path for proving
GET  /v1/tree/{pool}/root                  → current root + block anchor
GET  /v1/notes?since_cursor=&pool=         → encrypted note blobs (ciphertext)
GET  /v1/notes/stream                      → SSE stream of new notes
POST /v1/issuer/credentials                → issuer-signed credential delivery (ciphertext)
GET  /v1/credentials?recipient_hint=       → recipient pickup of encrypted credentials
GET  /v1/issuers                           → active issuer registry mirror
GET  /v1/stats                             → public protocol stats (TVL, counts)
GET  /v1/artifacts/{circuit}/{version}     → prover artifact manifest (sha256 hashes)
GET  /v1/artifacts/{circuit}/{version}/{f} → proving key / WASM prover bytes
```

## Development setup

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
  end-to-end.
- **Decoupled processes.** API and indexer share only the database; either
  can restart independently.
- **Integrity-checkable artifacts.** Proving keys and WASM provers are served
  with a manifest of SHA-256 hashes and an `x-artifact-sha256` response
  header, so clients verify before proving.

## Status / TODO toward M2 (shielded pool MVP)

- [x] Workspace, schema, migrations, docker compose, container image
- [x] Tree endpoints, note relay + SSE, issuer gateway, stats, artifacts CDN
- [x] Indexer loop with cursor persistence and idempotent, replayable ingest
- [ ] Real XDR (`ScVal`) event decoding once the pool contract's event layout
      freezes (`crates/attesta-indexer/src/events.rs` — currently a JSON shim
      so the ingest path is testable today)
- [ ] Poseidon-over-BLS12-381 tree hasher matching the circuits
      (`crates/attesta-core/src/merkle.rs` — currently a SHA-256 placeholder
      behind the `TreeHasher` trait; **security-critical**, dual review)
- [ ] Issuer signature verification on credential delivery (blocked on the
      M5 credential envelope format)
- [ ] Cached-frontier Merkle tree (current implementation rebuilds from
      leaves per request; fine for testnet scale)
- [ ] Disclosure trial-decryption + per-entry Merkle path verification
      (blocked on the M3 note encryption format)

## Contributing

Issues are labeled `backend/good-first-issue` and by difficulty. Anything
touching the no-secrets-server invariant is `security-critical` and requires
dual review. See the main project README for the full contribution guide and
roadmap (M1–M7).

## License

Apache-2.0
