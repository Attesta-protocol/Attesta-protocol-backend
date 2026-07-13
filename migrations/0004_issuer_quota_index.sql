-- Supports the per-issuer hourly delivery quota's sliding-window count
-- (Issue 7): SELECT count(*) ... WHERE issuer_id = $1 AND created_at > ...
CREATE INDEX credential_deliveries_issuer_created_idx
    ON credential_deliveries (issuer_id, created_at);
