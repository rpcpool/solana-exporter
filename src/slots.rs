//! Statistics of skipped and validated slots.

use crate::config::Whitelist;
use log::debug;
use prometheus_exporter::prometheus::{GaugeVec, IntCounterVec};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcBlockProductionConfig;
use solana_commitment_config::CommitmentConfig;
use std::collections::HashMap;
use std::fmt::{self, Display, Formatter};

/// The monitor of skipped and validated slots per validator with minimal internal state.
///
/// Each cycle issues a single unfiltered `getBlockProduction` call, which the
/// RPC node answers from its own epoch-to-date production tracking: a map of
/// node identity pubkey to `(leader slots, blocks produced)` for the current
/// epoch. That one cheap call covers the whole epoch so far, so there is no
/// leader-schedule download, no `getBlocks` range scanning, and no cold-start
/// backfill against the RPC's long-term block store.
pub struct SkippedSlotsMonitor<'a> {
    /// Shared Solana RPC client.
    client: &'a RpcClient,
    /// Prometheus counter.
    leader_slots: &'a IntCounterVec,
    /// Prometheus gauge.
    skipped_slot_percent: &'a GaugeVec,
    /// `range.first_slot` of the last `getBlockProduction` snapshot. Identifies
    /// the epoch the baseline below belongs to; taken from the response itself
    /// so an epoch rollover mid-cycle cannot skew the baseline.
    epoch_first_slot: u64,
    /// Last observed `(leader slots, blocks produced)` per identity, used to
    /// increment the counters by the per-cycle delta.
    last_production: HashMap<String, (usize, usize)>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SlotStatus {
    Skipped,
    Validated,
}

impl Display for SlotStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let s = match self {
            SlotStatus::Skipped => "skipped",
            SlotStatus::Validated => "validated",
        };
        write!(f, "{}", s)
    }
}

impl<'a> SkippedSlotsMonitor<'a> {
    /// Constructs a monitor given `client`.
    pub fn new(
        client: &'a RpcClient,
        leader_slots: &'a IntCounterVec,
        skipped_slot_percent: &'a GaugeVec,
    ) -> Self {
        Self {
            client,
            leader_slots,
            skipped_slot_percent,
            epoch_first_slot: 0,
            last_production: HashMap::new(),
        }
    }

    /// Exports the skipped slot statistics for the current epoch.
    pub async fn export_skipped_slots(&mut self, node_whitelist: &Whitelist) -> anyhow::Result<()> {
        // Pin the query to `finalized`. With an unset commitment and `range: None`
        // the node derives `last_slot = bank.slot()` for whichever (often
        // unrooted, tip) bank it picks, then bound-checks that range against its
        // own SlotHistory sysvar. On testnet that bank's slot routinely runs
        // ahead of the newest slot in history (skipped slots, forks, snapshot
        // restarts), so the node rejects its own computed range with
        // "lastSlot ... is too large" / "No slot history". A finalized bank's
        // slot is rooted and therefore present in that same bank's SlotHistory,
        // so the self-check cannot trip. `range: None` still scopes the response
        // to the current epoch.
        let production = self
            .client
            .get_block_production_with_config(RpcBlockProductionConfig {
                identity: None,
                range: None,
                commitment: Some(CommitmentConfig::finalized()),
            })
            .await?
            .value;

        if production.range.first_slot != self.epoch_first_slot {
            // New epoch: production numbers restart from zero, so the counter
            // baseline must too.
            self.epoch_first_slot = production.range.first_slot;
            self.last_production.clear();
            debug!(
                "SkippedSlotsMonitor reset for epoch starting at slot {}",
                self.epoch_first_slot
            );
        }

        let mut snapshot = HashMap::new();
        let mut feed = self.leader_slots.local();
        for (identity, (leader_slots, blocks_produced)) in production.by_identity {
            if !node_whitelist.contains(&identity) {
                continue;
            }

            let (prev_leader_slots, prev_blocks_produced) = self
                .last_production
                .get(&identity)
                .copied()
                .unwrap_or_default();
            // Saturating arithmetic absorbs the occasional regression when a
            // load-balanced RPC pool answers consecutive calls from nodes with
            // slightly different views of the epoch.
            let delta_validated = blocks_produced.saturating_sub(prev_blocks_produced);
            let delta_skipped = leader_slots
                .saturating_sub(prev_leader_slots)
                .saturating_sub(delta_validated);
            feed.with_label_values(&[&identity, &SlotStatus::Validated.to_string()])
                .inc_by(delta_validated as u64);
            feed.with_label_values(&[&identity, &SlotStatus::Skipped.to_string()])
                .inc_by(delta_skipped as u64);

            // The percentage is set from the epoch-to-date absolutes rather
            // than the counters, so it is exact regardless of counter resets.
            if leader_slots > 0 {
                let skipped = leader_slots - blocks_produced.min(leader_slots);
                let skipped_percent = (skipped as f64 / leader_slots as f64) * 100.0;
                self.skipped_slot_percent
                    .get_metric_with_label_values(&[&identity])
                    .map(|c| c.set(skipped_percent))?;
            }

            snapshot.insert(identity, (leader_slots, blocks_produced));
        }
        feed.flush();
        self.last_production = snapshot;

        debug!("Exported leader slots");
        Ok(())
    }
}
