use crate::geolocation::api::MaxMindAPIKey;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;

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
    /// Whitelisted vote account pubkeys.
    pub vote_account_whitelist: Option<Whitelist>,
    /// Whitelisted staking account pubkeys for APY calculation
    pub staking_account_whitelist: Option<Whitelist>,
    /// Whjether to process rewards data or not
    pub enable_rewards: Option<bool>,
    /// Whjether to process skipped slots data or not
    pub enable_skipped_slots: Option<bool>,
    /// Whether to export cluster-wide gossip node info (`solana_gossip_node_info`),
    /// one series per cluster node. Unlike the other metrics this is NOT filtered
    /// by the vote-account whitelist, so it adds one series per network node
    /// (thousands). Defaults to `false`.
    pub enable_gossip_node_info: Option<bool>,
    /// Whether to export the cluster-wide `solana_validator_node_info` enrichment
    /// metric (identity -> vote account, version, client, country, ASN, DZ edge)
    /// and `solana_validator_node_stake`. Covers every cluster node (thousands) and
    /// enables the ip-api geolocation background task. Defaults to `false`.
    pub enable_validator_node_info: Option<bool>,
    /// ip-api base URL used for enrichment geolocation. Defaults to the free tier
    /// endpoint (`http://ip-api.com`).
    pub ip_api_base_url: Option<String>,
    /// Whether to resolve DoubleZero edge membership for the `dz_edge` label on
    /// `solana_validator_node_info`. Adds egress to the DZ publisher endpoint.
    /// Defaults to `false`.
    pub enable_dz_edge: Option<bool>,
    /// DoubleZero publisher-check endpoint used when `enable_dz_edge` is set.
    pub dz_publisher_url: Option<String>,
    /// Maxmind API username and password. Serialized last: it is a TOML table
    /// (`[maxmind]`) and TOML requires all scalar values before any table.
    pub maxmind: Option<MaxMindAPIKey>,
}
