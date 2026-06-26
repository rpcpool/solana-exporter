use crate::config::Whitelist;
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use solana_client::{
    nonblocking::rpc_client::RpcClient, rpc_request::RpcRequest, rpc_response::RpcVoteAccountStatus,
};
use solana_clock::Epoch;

/// A cluster node as returned by `getClusterNodes`, preserving the gossip-table
/// address fields that the typed `RpcContactInfo` (solana-client 4.0.0) drops —
/// notably `tvu`, which is required to correlate shred (TVU) traffic back to a
/// node identity. Captured directly from the raw RPC JSON so we don't issue a
/// second `getClusterNodes` call against the (large) cluster every cycle.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GossipNode {
    /// Node identity pubkey, base-58.
    pub pubkey: String,
    /// Gossip socket address (`ip:port`), if advertised.
    pub gossip: Option<String>,
    /// TVU (shred-ingress) socket address (`ip:port`), if advertised.
    pub tvu: Option<String>,
    /// TPU socket address (`ip:port`), if advertised.
    pub tpu: Option<String>,
    /// Software version, if advertised.
    pub version: Option<String>,
}

/// Parses an already-fetched `getClusterNodes` JSON payload (as returned by
/// [`get_cluster_nodes_raw`]) into [`GossipNode`]s. Nodes that fail to
/// deserialize individually are skipped rather than failing the whole batch.
pub fn parse_gossip_nodes(raw: &Value) -> Vec<GossipNode> {
    raw.as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| serde_json::from_value::<GossipNode>(node.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Fetches the raw `getClusterNodes` JSON, tolerating a non-conformant
/// `clientId` field.
///
/// solana-client 4.0.0 added `RpcContactInfo.client_id: Option<String>` and
/// parses it strictly as a string. Some networks (e.g. doublezero) return
/// `clientId` as an integer, which makes the typed deserialization fail for the
/// *entire* response. We coerce any non-string, non-null `clientId` to its
/// string form before returning the raw `Value`, so callers can deserialize it
/// into multiple typed views (e.g. `RpcContactInfo` and [`GossipNode`]) without
/// re-issuing the RPC call.
pub async fn get_cluster_nodes_raw(client: &RpcClient) -> anyhow::Result<Value> {
    let mut raw: Value = client
        .send(RpcRequest::GetClusterNodes, Value::Null)
        .await
        .context("getClusterNodes RPC call failed")?;

    if let Some(nodes) = raw.as_array_mut() {
        for node in nodes {
            if let Some(client_id) = node.get_mut("clientId") {
                if !client_id.is_string() && !client_id.is_null() {
                    *client_id = Value::String(client_id.to_string());
                }
            }
        }
    }

    Ok(raw)
}

/// Returns the slot of the first confirmed block in `epoch`, if any.
pub async fn first_block_in_epoch(client: &RpcClient, epoch: Epoch) -> anyhow::Result<Option<u64>> {
    let epoch_schedule = client.get_epoch_schedule().await?;
    let first_slot = epoch_schedule.get_first_slot_in_epoch(epoch);

    Ok(client
        .get_blocks_with_limit(first_slot, 1)
        .await?
        .first()
        .cloned())
}

/// Maps vote pubkeys to node pubkeys based on the information provided in `vote_accounts`.
pub fn node_pubkeys(vote_pubkeys: &Whitelist, vote_accounts: &RpcVoteAccountStatus) -> Whitelist {
    if vote_pubkeys.0.is_empty() {
        Whitelist::default()
    } else {
        Whitelist(
            vote_accounts
                .current
                .iter()
                .chain(vote_accounts.delinquent.iter())
                .filter(|account| vote_pubkeys.contains(&account.vote_pubkey))
                .map(|account| account.node_pubkey.clone())
                .collect(),
        )
    }
}
