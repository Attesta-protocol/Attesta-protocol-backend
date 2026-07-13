# Operating the Attesta backend

## Health probes

| Endpoint | Meaning | Failure |
|---|---|---|
| `GET /health/live` (alias `/health`) | process is up; never touches the DB | process restart |
| `GET /health/ready` | `SELECT 1` against Postgres (migrations ran at startup, so reachable = migrated); optionally indexer cursor freshness | 503 with `{"failing": "database" \| "indexer_staleness"}`; recovers without a restart |

Set `READY_MAX_INDEXER_STALENESS_SECS` (e.g. `60`) to make readiness also
fail when no indexer cursor has advanced recently — useful when the API
and indexer deploy as one unit. Leave `0` for API-only deployments.

The Docker image's `HEALTHCHECK` and the compose `api` service probe
`/health/ready`; the compose `indexer` service disables it (no HTTP
server by default).

## Metrics

API: `GET /metrics` on the main port (Prometheus exposition).
Indexer: opt-in — set `INDEXER_METRICS_ADDR=0.0.0.0:9464` to start a
scrape listener on a separate port; unset means no listener at all.

**Invariant:** metrics carry pool ids, contract ids, route patterns,
counts, and timings only. No recipient hints, commitments, ciphertext,
or any per-user value may ever appear in a metric name or label.

### API series

- `attesta_api_requests_total{route,method,status}` — counter
- `attesta_api_request_duration_seconds{route,method,status}` — histogram
- `attesta_api_sse_subscribers` — gauge, live SSE connections
- `attesta_api_tree_lock_wait_seconds` — histogram, shared tree-lock wait
- `attesta_api_tree_topup_duration_seconds` — histogram, per-request tree top-up
- `attesta_api_tree_leaves{pool}` — gauge, leaves in the served tree

### Indexer series

- `attesta_indexer_lag_ledgers{contract}` — gauge, chain head minus
  cursor; ~0 when caught up, grows when stuck or outpaced
- `attesta_indexer_events_decoded_total{contract}` — counter
- `attesta_indexer_events_undecodable_total{contract}` — counter; any
  growth means event-layout drift or corrupt input (see ISSUES.md #2)
- `attesta_indexer_sync_errors_total{contract}` — counter (RPC/DB
  failures; the loop retries forever, this makes that visible)
- `attesta_indexer_sync_duration_seconds{contract}` — histogram

### Prometheus scrape config

```yaml
scrape_configs:
  - job_name: attesta-api
    static_configs: [{ targets: ["api:8080"] }]
  - job_name: attesta-indexer
    static_configs: [{ targets: ["indexer:9464"] }]
```

## Starter alerts

- **Indexer lag**: `attesta_indexer_lag_ledgers > 100 for 5m` — chain is
  outpacing ingest, or the loop is wedged on a poison event.
- **Undecodable events**: `increase(attesta_indexer_events_undecodable_total[15m]) > 0`
  — layout drift; treat as a bug, not noise.
- **Sync errors**: `increase(attesta_indexer_sync_errors_total[15m]) > 10`
  — RPC or database trouble.
- **5xx rate**: `sum(rate(attesta_api_requests_total{status=~"5.."}[5m])) > 0.1`.
- **Readiness flapping**: probe `/health/ready` from your uptime checker;
  a 503 with `indexer_staleness` while `/health/live` is 200 usually
  means the indexer died or lost its RPC.
- **SSE saturation**: `attesta_api_sse_subscribers` near
  `RATE_LIMIT_SSE_GLOBAL` — raise the cap or investigate a client leak.
