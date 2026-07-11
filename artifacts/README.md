# Prover artifacts

Versioned proving keys and WASM prover binaries served by
`GET /v1/artifacts/{circuit}/{version}` (manifest) and
`GET /v1/artifacts/{circuit}/{version}/{file}` (bytes, with
`x-artifact-sha256` header).

Layout:

```
artifacts/
  transfer/
    v1/
      manifest.json     # file list + sha256 hashes + circuit metadata
      prover.wasm
      proving_key.zkey
```

`manifest.json` example:

```json
{
  "circuit": "transfer",
  "version": "v1",
  "protocol": "groth16",
  "curve": "bls12-381",
  "files": {
    "prover.wasm":     { "sha256": "<hex>", "bytes": 0 },
    "proving_key.zkey": { "sha256": "<hex>", "bytes": 0 }
  },
  "verifying_key_onchain": "<contract id holding the VK>",
  "setup_ceremony": "<link to published transcript>"
}
```

Binary artifacts are gitignored; they are published to this directory by the
release pipeline after the trusted-setup ceremony (M1/M7). Clients MUST check
the manifest hash before proving.
