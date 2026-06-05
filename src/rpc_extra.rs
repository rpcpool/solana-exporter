use crate::config::Whitelist;
use serde_json::json;
use solana_client::{
    client_error::Result as ClientResult, rpc_client::RpcClient, rpc_config::RpcBlockConfig,
    rpc_request::RpcRequest, rpc_response::RpcVoteAccountStatus,
};
use solana_sdk::clock::{Epoch, Slot};
use solana_transaction_status::{EncodedConfirmedBlock, UiConfirmedBlock, UiTransactionEncoding};

// The 1.7.x `RpcClient` routes the block methods through `maybe_map_request`,
// which downgrades `getBlock`/`getBlocks`/`getBlocksWithLimit` to the removed
// `getConfirmed*` names whenever the node reports a version < 1.7.0. Alpenglow
// reports `solana-core` "0.4.1", so every such call becomes a -32601
// "Method not found". These thin wrappers call `send` directly with the modern
// request variant, bypassing the version remap. They mirror the upstream
// 1.7.9 method bodies exactly (minus the `maybe_map_request` wrapper).

/// `RpcClient::get_blocks_with_limit` without the version-based method remap.
pub fn get_blocks_with_limit(
    client: &RpcClient,
    start_slot: Slot,
    limit: usize,
) -> ClientResult<Vec<Slot>> {
    client.send(RpcRequest::GetBlocksWithLimit, json!([start_slot, limit]))
}

/// `RpcClient::get_blocks` without the version-based method remap.
pub fn get_blocks(
    client: &RpcClient,
    start_slot: Slot,
    end_slot: Option<Slot>,
) -> ClientResult<Vec<Slot>> {
    client.send(RpcRequest::GetBlocks, json!([start_slot, end_slot]))
}

/// `RpcClient::get_block` without the version-based method remap.
pub fn get_block(client: &RpcClient, slot: Slot) -> ClientResult<EncodedConfirmedBlock> {
    client.send(
        RpcRequest::GetBlock,
        json!([slot, UiTransactionEncoding::Json]),
    )
}

/// `RpcClient::get_block_with_config` without the version-based method remap.
pub fn get_block_with_config(
    client: &RpcClient,
    slot: Slot,
    config: RpcBlockConfig,
) -> ClientResult<UiConfirmedBlock> {
    client.send(RpcRequest::GetBlock, json!([slot, config]))
}

/// Applies `f` to the first block in `epoch`.
pub fn with_first_block<F, A>(client: &RpcClient, epoch: Epoch, f: F) -> anyhow::Result<Option<A>>
where
    F: Fn(u64) -> anyhow::Result<Option<A>>,
{
    let epoch_schedule = client.get_epoch_schedule()?;
    let first_slot = epoch_schedule.get_first_slot_in_epoch(epoch);

    // First block in `epoch`.
    let first_block = get_blocks_with_limit(client, first_slot, 1)?.get(0).cloned();

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
