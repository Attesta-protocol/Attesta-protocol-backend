# Credential mailbox lifecycle

Status: implemented (Issue 6). The envelope *contents* format is still an
M5 deliverable; nothing here depends on it.

## The problem

`credential_deliveries` is an anonymous mailbox: issuers deposit
ciphertext under an opaque, recipient-derived `recipient_hint`, and
recipients poll `GET /v1/credentials?recipient_hint=`. Before this design
landed, nothing ever set `claimed_at`, so every delivery was returned on
every pickup forever and the table grew without bound.

Recipients are deliberately anonymous — a hint is a mailbox tag, not an
identity — so "claiming" cannot require an account. But it also must not
let a third party who guesses (or observes) a hint mark someone else's
mail claimed, which would be a silent denial-of-delivery attack.

## Claim tokens, not hint-based claiming

At delivery time the issuer includes `claim_token_hash =
SHA-256(claim_token)`, where the `claim_token` itself travels **inside the
encrypted payload**. Only the true recipient can decrypt the payload,
recover the token, and present it:

```
POST /v1/credentials/{delivery_id}/claim   { "claim_token": "<base64>" }
```

The server hashes the presented token and compares against the stored
hash. This makes malicious claiming exactly as hard as breaking the
payload encryption, with no accounts and no new secrets held server-side
(a hash preimage is not credential material — the no-secrets invariant
holds).

Why not claim-on-pickup or hint-based claiming?

- **Claim-on-pickup**: anyone who knows a hint could sweep the mailbox
  empty before the recipient looks. Pickup must stay idempotent and
  read-only.
- **Hint-signed claiming**: hints are unauthenticated by design; there is
  no key to sign with without turning hints into identities.

Rules:

- Wrong or missing token → `403`, row untouched.
- Already claimed → `409` (idempotent retries can treat this as success).
- Deliveries made without a `claim_token_hash` (older issuers, optional
  field) can never be claimed explicitly; they age out via retention.

## Pickup pagination

`GET /v1/credentials?recipient_hint=&since_cursor=` pages by a monotonic
`seq` column with the same cursor contract as `/v1/notes`: pass the
returned `next_cursor` to fetch the next page; absent cursor means last
page. Unclaimed rows only.

## Retention

A background sweeper in the API deletes:

- claimed rows `CREDENTIAL_RETENTION_CLAIMED_DAYS` after `claimed_at`
  (default 30);
- unclaimed rows `CREDENTIAL_RETENTION_UNCLAIMED_DAYS` after `created_at`
  (default 180) — abandoned mailboxes and wrong-hint deliveries age out.

Either value set to `0` disables that deletion entirely (self-hosters may
want to keep everything). The sweep runs hourly; deletes are batched.
