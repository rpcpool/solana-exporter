use anyhow::Context;

use solana_sdk::account::Account;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;
use solana_transaction_status::{Reward, Rewards};
use std::collections::BTreeMap;

pub type AccountsInfo = BTreeMap<Pubkey, Option<Account>>;

/// Name of the caching database.
pub const EPOCH_REWARDS_CACHE_TREE_NAME: &str = "epoch_rewards_credit_cache";
pub const ACCOUNT_CACHE_TREE_NAME: &str = "account_cache";

/// A caching database for vote accounts' credit growth
pub struct RewardsCache {
    epoch_rewards_tree: sled::Tree,
    account_tree: sled::Tree,
}

impl RewardsCache {
    /// Creates a new cache using a tree.
    pub fn new(epoch_rewards_tree: sled::Tree, account_tree: sled::Tree) -> Self {
        Self {
            epoch_rewards_tree,
            account_tree,
        }
    }

    /// Adds a set of rewards of an epoch.
    pub fn add_epoch_rewards(&self, epoch: Epoch, rewards: &[Reward]) -> anyhow::Result<()> {
        // Insert into database
        self.epoch_rewards_tree
            .insert(epoch.to_be_bytes(), bincode::serialize(&rewards.to_vec())?)
            .context("could not insert epoch rewards into database")?;

        Ok(())
    }

    /// Returns the set of rewards of an epoch.
    pub fn get_epoch_rewards(&self, epoch: Epoch) -> anyhow::Result<Option<Rewards>> {
        self.epoch_rewards_tree
            .get(epoch.to_be_bytes())
            .context("could not fetch epoch rewards from database")?
            .map(|x| bincode::deserialize(&x))
            .transpose()
            .context("could not deserialize fetched epoch rewards")
    }

    /// Adds a set of account data of an epoch.
    // FIXME: Make sure this does not overwrite existing data.
    pub fn add_epoch_data(
        &self,
        epoch: Epoch,
        account_info: &[Option<Account>],
    ) -> anyhow::Result<()> {
        self.account_tree
            .insert(
                epoch.to_be_bytes(),
                bincode::serialize(&account_info.to_vec())?,
            )
            .context("could not insert new account data into database")?;
        Ok(())
    }

    /// Returns a set of account data of an epoch
    pub fn get_epoch_data(&self, epoch: Epoch) -> anyhow::Result<Option<AccountsInfo>> {
        self.account_tree
            .get(epoch.to_be_bytes())
            .context("could not fetch from database")?
            .map(|x| bincode::deserialize(&x))
            .transpose()
            .context("could not deserialize fetched data")
    }
}
