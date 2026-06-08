use crate::config::Whitelist;
use anyhow::Context;
use serde_json::Value;
use solana_client::{
    rpc_client::RpcClient,
    rpc_request::RpcRequest,
    rpc_response::{RpcContactInfo, RpcVoteAccountStatus},
};
use solana_clock::Epoch;

/// Fetches the cluster nodes, tolerating a non-conformant `clientId` field.
///
/// solana-client 4.0.0 added `RpcContactInfo.client_id: Option<String>` and
/// parses it strictly as a string. Some networks (e.g. doublezero) return
/// `clientId` as an integer, which makes `RpcClient::get_cluster_nodes` fail
/// deserialization of the *entire* response. We fetch the raw JSON and coerce
/// any non-string, non-null `clientId` to its string form before deserializing
/// into the typed `RpcContactInfo`.
pub fn get_cluster_nodes_lenient(client: &RpcClient) -> anyhow::Result<Vec<RpcContactInfo>> {
    let mut raw: Value = client
        .send(RpcRequest::GetClusterNodes, Value::Null)
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

    serde_json::from_value(raw).context("failed to deserialize getClusterNodes response")
}

/// Applies `f` to the first block in `epoch`.
pub fn with_first_block<F, A>(client: &RpcClient, epoch: Epoch, f: F) -> anyhow::Result<Option<A>>
where
    F: Fn(u64) -> anyhow::Result<Option<A>>,
{
    let epoch_schedule = client.get_epoch_schedule()?;
    let first_slot = epoch_schedule.get_first_slot_in_epoch(epoch);

    // First block in `epoch`.
    let first_block = client
        .get_blocks_with_limit(first_slot, 1)?
        .first()
        .cloned();

    if let Some(block) = first_block {
        f(block)
    } else {
        Ok(None)
    }
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
