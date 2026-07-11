//! Minimal Soroban JSON-RPC client — just what the indexer needs
//! (`getLatestLedger`, `getEvents` with cursor pagination).

use serde::{Deserialize, Serialize};
use serde_json::json;

pub struct SorobanClient {
    http: reqwest::Client,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestLedger {
    pub sequence: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsPage {
    #[serde(default)]
    pub events: Vec<RawEvent>,
    pub cursor: Option<String>,
    #[serde(default)]
    pub latest_ledger: u64,
}

/// One contract event as returned by `getEvents`. Topics and value are
/// base64 XDR (`ScVal`); decoding happens in `events.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEvent {
    pub id: String,
    #[serde(default)]
    pub contract_id: String,
    pub ledger: u64,
    #[serde(default)]
    pub tx_hash: String,
    #[serde(default)]
    pub topic: Vec<String>,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<serde_json::Value>,
}

impl SorobanClient {
    pub fn new(url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            url,
        }
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<T> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp: RpcResponse<T> = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(err) = resp.error {
            anyhow::bail!("rpc error from {method}: {err}");
        }
        resp.result
            .ok_or_else(|| anyhow::anyhow!("rpc response for {method} had no result"))
    }

    pub async fn latest_ledger(&self) -> anyhow::Result<LatestLedger> {
        self.call("getLatestLedger", json!({})).await
    }

    /// Fetch contract events for `contract_id`, resuming from `cursor` when
    /// present, otherwise from `start_ledger`.
    pub async fn get_events(
        &self,
        contract_id: &str,
        start_ledger: u64,
        cursor: Option<&str>,
    ) -> anyhow::Result<EventsPage> {
        let mut params = json!({
            "filters": [{ "type": "contract", "contractIds": [contract_id] }],
            "pagination": { "limit": 100 },
        });
        if let Some(c) = cursor {
            params["pagination"]["cursor"] = json!(c);
        } else {
            params["startLedger"] = json!(start_ledger.max(1));
        }
        self.call("getEvents", params).await
    }
}
