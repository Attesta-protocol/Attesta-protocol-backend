//! Ingest loop: poll events per configured contract, decode, store.
//! Idempotent by construction (ON CONFLICT DO NOTHING on unique keys),
//! so replays and cursor resets are always safe.

use std::time::Duration;

use attesta_core::config::Config;
use sqlx::PgPool;

use crate::{
    events::{decode, PoolEvent},
    rpc::SorobanClient,
};

pub async fn run(db: PgPool, client: SorobanClient, config: Config) -> anyhow::Result<()> {
    let poll = Duration::from_secs(config.indexer_poll_secs);
    let mut contracts = config.pool_contract_ids.clone();
    if let Some(reg) = &config.registry_contract_id {
        contracts.push(reg.clone());
    }

    loop {
        for contract_id in &contracts {
            let started = std::time::Instant::now();
            if let Err(e) = sync_contract(&db, &client, contract_id).await {
                tracing::warn!(contract = %contract_id, error = %e, "sync failed; will retry");
                metrics::counter!("attesta_indexer_sync_errors_total",
                    "contract" => contract_id.clone())
                .increment(1);
            }
            metrics::histogram!("attesta_indexer_sync_duration_seconds",
                "contract" => contract_id.clone())
            .record(started.elapsed().as_secs_f64());
        }
        tokio::time::sleep(poll).await;
    }
}

async fn sync_contract(
    db: &PgPool,
    client: &SorobanClient,
    contract_id: &str,
) -> anyhow::Result<()> {
    let (mut last_ledger, mut cursor): (i64, Option<String>) = sqlx::query_as(
        "SELECT last_ledger, last_cursor FROM indexer_cursors WHERE contract_id = $1",
    )
    .bind(contract_id)
    .fetch_optional(db)
    .await?
    .unwrap_or((0, None));

    loop {
        let page = client
            .get_events(contract_id, last_ledger as u64 + 1, cursor.as_deref())
            .await?;

        for raw in &page.events {
            if let Some(event) = decode(raw) {
                store_event(db, contract_id, raw.ledger as i64, &raw.tx_hash, event).await?;
                metrics::counter!("attesta_indexer_events_decoded_total",
                    "contract" => contract_id.to_owned())
                .increment(1);
            } else {
                // Undecodable events are a loud signal: either layout
                // drift against the deployed contracts or corrupt input.
                metrics::counter!("attesta_indexer_events_undecodable_total",
                    "contract" => contract_id.to_owned())
                .increment(1);
            }
            last_ledger = last_ledger.max(raw.ledger as i64);
        }

        let done = page.events.is_empty();
        cursor = page.cursor.clone();
        if done {
            last_ledger = last_ledger.max(page.latest_ledger as i64);
        }

        // Ingest lag: how far the cursor trails the chain head. ~0 when
        // caught up; grows when the RPC outpaces us (or we are stuck).
        if page.latest_ledger > 0 {
            metrics::gauge!("attesta_indexer_lag_ledgers",
                "contract" => contract_id.to_owned())
            .set((page.latest_ledger as i64 - last_ledger).max(0) as f64);
        }

        sqlx::query(
            "INSERT INTO indexer_cursors (contract_id, last_ledger, last_cursor, updated_at)
             VALUES ($1, $2, $3, now())
             ON CONFLICT (contract_id)
             DO UPDATE SET last_ledger = $2, last_cursor = $3, updated_at = now()",
        )
        .bind(contract_id)
        .bind(last_ledger)
        .bind(&cursor)
        .execute(db)
        .await?;

        if done {
            return Ok(());
        }
    }
}

async fn store_event(
    db: &PgPool,
    contract_id: &str,
    ledger: i64,
    tx_hash: &str,
    event: PoolEvent,
) -> anyhow::Result<()> {
    match event {
        PoolEvent::NewCommitment {
            commitment,
            leaf_index,
            deposit_amount,
            asset,
        } => {
            sqlx::query(
                "INSERT INTO commitments (pool, leaf_index, commitment, ledger, tx_hash)
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
            )
            .bind(contract_id)
            .bind(leaf_index)
            .bind(commitment.as_slice())
            .bind(ledger)
            .bind(tx_hash)
            .execute(db)
            .await?;

            if let (Some(amount), Some(asset)) = (deposit_amount, asset) {
                add_pool_total(db, contract_id, &asset, amount, 0).await?;
            }
        }
        PoolEvent::NullifierSpent { nullifier } => {
            sqlx::query(
                "INSERT INTO nullifiers (pool, nullifier, ledger, tx_hash)
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
            .bind(contract_id)
            .bind(nullifier.as_slice())
            .bind(ledger)
            .bind(tx_hash)
            .execute(db)
            .await?;
        }
        PoolEvent::EncryptedNote {
            commitment,
            ephemeral_pubkey,
            ciphertext,
        } => {
            sqlx::query(
                "INSERT INTO encrypted_notes
                     (pool, commitment, ephemeral_pubkey, ciphertext, ledger, tx_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(contract_id)
            .bind(commitment.as_slice())
            .bind(&ephemeral_pubkey)
            .bind(&ciphertext)
            .bind(ledger)
            .bind(tx_hash)
            .execute(db)
            .await?;
        }
        PoolEvent::Withdrawal { amount, asset } => {
            add_pool_total(db, contract_id, &asset, 0, amount).await?;
        }
        PoolEvent::IssuerUpdated {
            issuer_id,
            name,
            public_key,
            claim_types,
            status,
        } => {
            sqlx::query(
                "INSERT INTO issuers
                     (issuer_id, name, public_key, claim_types, status, registered_ledger)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (issuer_id) DO UPDATE SET
                     name = $2, public_key = $3, claim_types = $4,
                     status = $5, updated_at = now()",
            )
            .bind(&issuer_id)
            .bind(&name)
            .bind(&public_key)
            .bind(&claim_types)
            .bind(&status)
            .bind(ledger)
            .execute(db)
            .await?;
        }
    }
    Ok(())
}

async fn add_pool_total(
    db: &PgPool,
    pool: &str,
    asset: &str,
    inflow: i128,
    outflow: i128,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO pool_totals (pool, asset, total_in, total_out)
         VALUES ($1, $2, $3::numeric, $4::numeric)
         ON CONFLICT (pool) DO UPDATE SET
             total_in = pool_totals.total_in + $3::numeric,
             total_out = pool_totals.total_out + $4::numeric,
             updated_at = now()",
    )
    .bind(pool)
    .bind(asset)
    .bind(inflow.to_string())
    .bind(outflow.to_string())
    .execute(db)
    .await?;
    Ok(())
}
