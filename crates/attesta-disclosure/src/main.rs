//! Disclosure CLI: builds auditor disclosure packages from a user-supplied
//! *scoped viewing key*.
//!
//! Runs entirely locally: it fetches public data (encrypted notes,
//! commitments, roots) from any Attesta backend — including a self-hosted
//! one — and does all decryption on this machine. The viewing key is read
//! from a local file and is never sent anywhere.
//!
//! Output: a JSON report of the account's payment history plus the Merkle
//! data an auditor needs to independently check each entry against
//! on-chain commitments.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

#[derive(Parser)]
#[command(name = "disclosure", about = "Attesta auditor disclosure package tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a disclosure report for one account, scoped by a viewing key.
    Generate {
        /// Backend URL to fetch public data from (self-hosted works too).
        #[arg(long, env = "ATTESTA_API_URL", default_value = "http://localhost:8080")]
        api_url: String,
        /// Shielded pool contract id.
        #[arg(long)]
        pool: String,
        /// Path to the scoped viewing key file (stays local, never uploaded).
        #[arg(long)]
        viewing_key: PathBuf,
        /// Where to write the report JSON.
        #[arg(long, default_value = "disclosure-report.json")]
        output: PathBuf,
    },
    /// Verify a previously generated report against a backend / the chain.
    Verify {
        #[arg(long, env = "ATTESTA_API_URL", default_value = "http://localhost:8080")]
        api_url: String,
        /// Path to a disclosure-report.json.
        report: PathBuf,
    },
}

#[derive(Serialize)]
struct Report {
    version: u32,
    pool: String,
    generated_at: String,
    tree_root: String,
    anchored_ledger: i64,
    /// Decrypted entries for THIS viewing key's scope only.
    entries: Vec<serde_json::Value>,
    total_notes_scanned: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Generate {
            api_url,
            pool,
            viewing_key,
            output,
        } => generate(api_url, pool, viewing_key, output).await,
        Command::Verify { api_url, report } => verify(api_url, report).await,
    }
}

async fn generate(
    api_url: String,
    pool: String,
    viewing_key: PathBuf,
    output: PathBuf,
) -> anyhow::Result<()> {
    let _key = std::fs::read(&viewing_key)
        .with_context(|| format!("reading viewing key {}", viewing_key.display()))?;

    let http = reqwest::Client::new();

    let root: serde_json::Value = http
        .get(format!("{api_url}/v1/tree/{pool}/root"))
        .send()
        .await?
        .error_for_status()
        .context("fetching tree root (is the pool indexed yet?)")?
        .json()
        .await?;

    // Page through every encrypted note for the pool.
    let mut notes: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<i64> = None;
    loop {
        let mut req = http
            .get(format!("{api_url}/v1/notes"))
            .query(&[("pool", pool.as_str())]);
        if let Some(c) = cursor {
            req = req.query(&[("since_cursor", c)]);
        }
        let page: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
        if let Some(items) = page["notes"].as_array() {
            notes.extend(items.iter().cloned());
        }
        match page["next_cursor"].as_i64() {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }

    // TODO(M4): trial-decrypt each note with the scoped viewing key and,
    // for successes, attach the decrypted memo/amount plus the Merkle path
    // of its commitment. Blocked on the note encryption format (M3).
    let entries: Vec<serde_json::Value> = Vec::new();

    let report = Report {
        version: 1,
        pool,
        generated_at: chrono::Utc::now().to_rfc3339(),
        tree_root: root["root"].as_str().unwrap_or_default().to_string(),
        anchored_ledger: root["anchored_ledger"].as_i64().unwrap_or_default(),
        total_notes_scanned: notes.len(),
        entries,
    };

    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "wrote {} (scanned {} notes; decryption pending M3 note format)",
        output.display(),
        report.total_notes_scanned
    );
    Ok(())
}

async fn verify(api_url: String, report_path: PathBuf) -> anyhow::Result<()> {
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&report_path)
            .with_context(|| format!("reading {}", report_path.display()))?,
    )?;
    let pool = report["pool"].as_str().context("report missing pool")?;

    let live: serde_json::Value = reqwest::Client::new()
        .get(format!("{api_url}/v1/tree/{pool}/root"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // TODO(M4): re-verify each entry's Merkle path against the anchored
    // root, not just compare current roots.
    let matches = live["root"] == report["tree_root"];
    println!(
        "{}",
        json!({
            "report_root": report["tree_root"],
            "live_root": live["root"],
            "roots_match": matches,
        })
    );
    Ok(())
}
