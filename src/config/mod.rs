use crate::geolocation::api::MaxMindAPIKey;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

const DEFAULT_UPDATE_INTERVAL_SECONDS: u64 = 1;

fn default_update_interval_seconds() -> u64 {
    DEFAULT_UPDATE_INTERVAL_SECONDS
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Whitelist(pub HashSet<String>);

impl Whitelist {
    pub fn contains(&self, value: &str) -> bool {
        self.0.is_empty() || self.0.contains(value)
    }
}

pub const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExporterConfig {
    /// Solana RPC address.
    pub rpc: String,
    /// Prometheus target socket address.
    pub target: SocketAddr,
    /// Delay between completed metric update cycles. Defaults to one second.
    #[serde(default = "default_update_interval_seconds")]
    pub update_interval_seconds: u64,
    /// Whitelisted vote account pubkeys.
    pub vote_account_whitelist: Option<Whitelist>,
    /// Whitelisted staking account pubkeys for APY calculation
    pub staking_account_whitelist: Option<Whitelist>,
    /// Maxmind API username and password.
    pub maxmind: Option<MaxMindAPIKey>,
    /// Whjether to process rewards data or not
    pub enable_rewards: Option<bool>,
    /// Whjether to process skipped slots data or not
    pub enable_skipped_slots: Option<bool>,
    /// Whether to export cluster-wide gossip node info (`solana_gossip_node_info`),
    /// one series per cluster node. Unlike the other metrics this is NOT filtered
    /// by the vote-account whitelist, so it adds one series per network node
    /// (thousands). Defaults to `false`.
    pub enable_gossip_node_info: Option<bool>,
    /// Whether to export the reward destinations of whitelisted vote accounts
    /// (`solana_vote_account_reward_destination`). Requires a vote account layout
    /// v4 or later on chain. Defaults to `false`.
    pub enable_vote_account_destinations: Option<bool>,
}

impl ExporterConfig {
    pub fn update_interval(&self) -> anyhow::Result<Duration> {
        anyhow::ensure!(
            self.update_interval_seconds > 0,
            "update_interval_seconds must be greater than zero"
        );
        Ok(Duration::from_secs(self.update_interval_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(update_interval: &str) -> ExporterConfig {
        toml::from_str(&format!(
            "rpc = 'http://localhost:8899'\ntarget = '127.0.0.1:9179'\n{update_interval}"
        ))
        .unwrap()
    }

    #[test]
    fn update_interval_defaults_to_one_second() {
        assert_eq!(
            config("").update_interval().unwrap(),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn update_interval_is_configurable() {
        assert_eq!(
            config("update_interval_seconds = 15")
                .update_interval()
                .unwrap(),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn update_interval_rejects_zero() {
        assert!(config("update_interval_seconds = 0")
            .update_interval()
            .is_err());
    }
}
