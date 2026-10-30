use crate::config::Whitelist;
use anyhow::Context;
use log::warn;
use serde::Deserialize;
use serde_json::Value;
use solana_client::{
    nonblocking::rpc_client::RpcClient, rpc_request::RpcRequest, rpc_response::RpcVoteAccountStatus,
};
use solana_clock::Epoch;
use solana_pubkey::Pubkey;
use std::convert::TryInto;
use std::str::FromStr;

/// Program id that owns vote accounts.
const VOTE_PROGRAM_ID: &str = "Vote111111111111111111111111111111111111111";
/// Vote state version written for vote account v4. Layout from agave's
/// `vote/src/vote_state_view/frame_v4.rs`.
const VOTE_STATE_V4_VERSION: u32 = 3;
/// Byte offset of `inflation_rewards_collector` in v4 vote account data.
const INFLATION_REWARDS_COLLECTOR_OFFSET: usize = 68;
/// Byte offset of `block_revenue_collector` in v4 vote account data.
const BLOCK_REVENUE_COLLECTOR_OFFSET: usize = 100;
/// Accounts per `getMultipleAccounts` call.
const GET_MULTIPLE_ACCOUNTS_CHUNK: usize = 100;

/// Reward destinations of a vote account, from vote account v4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewardDestinations {
    /// Receives the inflation rewards.
    pub inflation_rewards_collector: Pubkey,
    /// Receives the block revenue.
    pub block_revenue_collector: Pubkey,
}

/// Reads the reward destinations from raw vote account data.
///
/// Returns `Ok(None)` for vote states older than v4, which have no destinations.
/// Returns an error for a version newer than v4, because its layout is unknown.
pub fn parse_reward_destinations(data: &[u8]) -> anyhow::Result<Option<RewardDestinations>> {
    let version_bytes: [u8; 4] = data
        .get(..4)
        .context("vote account data is too short for a version")?
        .try_into()?;
    let version = u32::from_le_bytes(version_bytes);
    if version < VOTE_STATE_V4_VERSION {
        return Ok(None);
    }
    if version > VOTE_STATE_V4_VERSION {
        anyhow::bail!("unsupported vote state version {version}");
    }

    let pubkey_at = |offset: usize| -> anyhow::Result<Pubkey> {
        let bytes: [u8; 32] = data
            .get(offset..offset + 32)
            .context("vote account data is too short for reward destinations")?
            .try_into()?;
        Ok(Pubkey::new_from_array(bytes))
    };

    Ok(Some(RewardDestinations {
        inflation_rewards_collector: pubkey_at(INFLATION_REWARDS_COLLECTOR_OFFSET)?,
        block_revenue_collector: pubkey_at(BLOCK_REVENUE_COLLECTOR_OFFSET)?,
    }))
}

/// Fetches the reward destinations of the given vote accounts.
///
/// Skips accounts that do not exist, are not owned by the vote program, or have
/// a vote state this exporter cannot read. A failed batch returns an error.
pub async fn get_reward_destinations(
    client: &RpcClient,
    vote_pubkeys: &[Pubkey],
) -> anyhow::Result<Vec<(Pubkey, RewardDestinations)>> {
    let vote_program = Pubkey::from_str(VOTE_PROGRAM_ID)?;
    let mut destinations = Vec::with_capacity(vote_pubkeys.len());

    for chunk in vote_pubkeys.chunks(GET_MULTIPLE_ACCOUNTS_CHUNK) {
        let accounts = client
            .get_multiple_accounts(chunk)
            .await
            .context("getMultipleAccounts RPC call failed")?;

        for (pubkey, account) in chunk.iter().zip(accounts) {
            let Some(account) = account else {
                continue;
            };
            if account.owner != vote_program {
                continue;
            }
            match parse_reward_destinations(&account.data) {
                Ok(Some(found)) => destinations.push((*pubkey, found)),
                Ok(None) => {}
                Err(e) => warn!("Skipping reward destinations of vote account {pubkey}: {e:#}"),
            }
        }
    }

    Ok(destinations)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use solana_vote_interface::state::{VoteStateV4, VoteStateVersions};

    fn v4_data(inflation: Pubkey, block: Pubkey) -> Vec<u8> {
        let state = VoteStateV4 {
            inflation_rewards_collector: inflation,
            block_revenue_collector: block,
            ..VoteStateV4::default()
        };
        bincode::serialize(&VoteStateVersions::new_v4(state)).unwrap()
    }

    #[test]
    fn parses_v4_reward_destinations_from_serialized_state() {
        let (inflation, block) = (Pubkey::new_unique(), Pubkey::new_unique());
        let parsed = parse_reward_destinations(&v4_data(inflation, block)).unwrap();
        assert_eq!(
            parsed,
            Some(RewardDestinations {
                inflation_rewards_collector: inflation,
                block_revenue_collector: block,
            })
        );
    }

    #[test]
    fn skips_pre_v4_vote_state() {
        let mut data = 2u32.to_le_bytes().to_vec();
        data.resize(3762, 0);
        assert_eq!(parse_reward_destinations(&data).unwrap(), None);
    }

    #[test]
    fn rejects_unknown_newer_version() {
        let mut data = 4u32.to_le_bytes().to_vec();
        data.resize(3762, 0);
        assert!(parse_reward_destinations(&data).is_err());
    }

    #[test]
    fn rejects_truncated_v4_data() {
        let data = v4_data(Pubkey::new_unique(), Pubkey::new_unique());
        assert!(parse_reward_destinations(&data[..131]).is_err());
        assert!(parse_reward_destinations(&data[..3]).is_err());
    }
}
