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
    /// Turns epoch-to-date production snapshots into counter increments.
    tracker: ProductionTracker,
}

/// Counter increments for one identity, with the epoch-to-date absolutes they came from.
#[derive(Debug, PartialEq, Eq)]
struct ProductionUpdate {
    identity: String,
    leader_slots: usize,
    blocks_produced: usize,
    new_validated: usize,
    new_skipped: usize,
}

/// Converts `getBlockProduction` snapshots into per-identity increments.
/// The first snapshot after start is a baseline and adds nothing.
#[derive(Debug, Default)]
struct ProductionTracker {
    /// `range.first_slot` of the tracked epoch. `None` until the first snapshot.
    epoch_first_slot: Option<u64>,
    /// Highest `(leader slots, blocks produced)` seen per identity in the tracked epoch.
    last_production: HashMap<String, (usize, usize)>,
}

impl ProductionTracker {
    /// Returns the increments since the previous snapshot, or `None` for a snapshot of an older epoch.
    fn observe(
        &mut self,
        epoch_first_slot: u64,
        by_identity: HashMap<String, (usize, usize)>,
    ) -> Option<Vec<ProductionUpdate>> {
        let is_baseline = match self.epoch_first_slot {
            None => true,
            Some(tracked) if epoch_first_slot < tracked => return None,
            Some(tracked) if epoch_first_slot > tracked => {
                // The epoch rolled over while running, so every slot of the new epoch is new.
                self.last_production.clear();
                false
            }
            Some(_) => false,
        };
        self.epoch_first_slot = Some(epoch_first_slot);

        let mut updates = Vec::with_capacity(by_identity.len());
        for (identity, (leader_slots, blocks_produced)) in by_identity {
            // An identity missing from the last snapshot had no leader slots then.
            let entry = self.last_production.entry(identity.clone()).or_default();
            let (prev_leader_slots, prev_blocks_produced) = *entry;
            // Keep the maximum, so an answer from a lagging RPC node is not counted twice.
            *entry = (
                leader_slots.max(prev_leader_slots),
                blocks_produced.max(prev_blocks_produced),
            );
            let (new_validated, new_skipped) = if is_baseline {
                (0, 0)
            } else {
                let validated = entry.1 - prev_blocks_produced;
                let skipped = (entry.0 - prev_leader_slots).saturating_sub(validated);
                (validated, skipped)
            };
            updates.push(ProductionUpdate {
                identity,
                leader_slots,
                blocks_produced,
                new_validated,
                new_skipped,
            });
        }
        Some(updates)
    }
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
            tracker: ProductionTracker::default(),
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

        let Some(updates) = self
            .tracker
            .observe(production.range.first_slot, production.by_identity)
        else {
            debug!(
                "Ignoring getBlockProduction snapshot of an older epoch (first slot {})",
                production.range.first_slot
            );
            return Ok(());
        };

        let mut feed = self.leader_slots.local();
        for update in updates {
            if !node_whitelist.contains(&update.identity) {
                continue;
            }
            feed.with_label_values(&[&update.identity, &SlotStatus::Validated.to_string()])
                .inc_by(update.new_validated as u64);
            feed.with_label_values(&[&update.identity, &SlotStatus::Skipped.to_string()])
                .inc_by(update.new_skipped as u64);

            // The percentage uses the epoch-to-date absolutes, so it does not depend on the counters.
            if update.leader_slots > 0 {
                let skipped = update.leader_slots - update.blocks_produced.min(update.leader_slots);
                let skipped_percent = (skipped as f64 / update.leader_slots as f64) * 100.0;
                self.skipped_slot_percent
                    .get_metric_with_label_values(&[&update.identity])
                    .map(|c| c.set(skipped_percent))?;
            }
        }
        feed.flush();

        debug!("Exported leader slots");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ProductionTracker, ProductionUpdate};
    use std::collections::HashMap;

    const EPOCH_1: u64 = 432_000;
    const EPOCH_2: u64 = 864_000;

    fn snapshot(entries: &[(&str, usize, usize)]) -> HashMap<String, (usize, usize)> {
        entries
            .iter()
            .map(|(id, leader, produced)| (id.to_string(), (*leader, *produced)))
            .collect()
    }

    /// Returns `identity -> (new validated, new skipped)`.
    fn increments(updates: Vec<ProductionUpdate>) -> HashMap<String, (usize, usize)> {
        updates
            .into_iter()
            .map(|u| (u.identity, (u.new_validated, u.new_skipped)))
            .collect()
    }

    #[test]
    fn first_snapshot_is_a_baseline() {
        let mut tracker = ProductionTracker::default();
        let updates = tracker
            .observe(EPOCH_1, snapshot(&[("a", 26_000, 25_900), ("b", 40, 40)]))
            .unwrap();
        let a = updates.iter().find(|u| u.identity == "a").unwrap();
        assert_eq!((a.leader_slots, a.blocks_produced), (26_000, 25_900));
        assert_eq!(increments(updates), snapshot(&[("a", 0, 0), ("b", 0, 0)]));
    }

    #[test]
    fn second_snapshot_counts_only_the_delta() {
        let mut tracker = ProductionTracker::default();
        tracker.observe(EPOCH_1, snapshot(&[("a", 100, 90)]));
        let updates = tracker
            .observe(EPOCH_1, snapshot(&[("a", 108, 96)]))
            .unwrap();
        // 8 new leader slots: 6 produced, 2 skipped.
        assert_eq!(increments(updates), snapshot(&[("a", 6, 2)]));
    }

    #[test]
    fn identity_first_seen_after_the_baseline_counts_from_zero() {
        let mut tracker = ProductionTracker::default();
        tracker.observe(EPOCH_1, snapshot(&[("a", 100, 90)]));
        let updates = tracker
            .observe(EPOCH_1, snapshot(&[("a", 100, 90), ("b", 4, 3)]))
            .unwrap();
        assert_eq!(increments(updates), snapshot(&[("a", 0, 0), ("b", 3, 1)]));
    }

    #[test]
    fn lagging_snapshot_is_not_counted_twice() {
        let mut tracker = ProductionTracker::default();
        tracker.observe(EPOCH_1, snapshot(&[("a", 100, 90)]));
        let lagging = tracker
            .observe(EPOCH_1, snapshot(&[("a", 96, 87)]))
            .unwrap();
        assert_eq!(increments(lagging), snapshot(&[("a", 0, 0)]));
        let caught_up = tracker
            .observe(EPOCH_1, snapshot(&[("a", 104, 94)]))
            .unwrap();
        assert_eq!(increments(caught_up), snapshot(&[("a", 4, 0)]));
    }

    #[test]
    fn epoch_rollover_counts_the_new_epoch_from_zero() {
        let mut tracker = ProductionTracker::default();
        tracker.observe(EPOCH_1, snapshot(&[("a", 26_000, 25_900)]));
        let updates = tracker.observe(EPOCH_2, snapshot(&[("a", 8, 7)])).unwrap();
        assert_eq!(increments(updates), snapshot(&[("a", 7, 1)]));
        let next = tracker
            .observe(EPOCH_2, snapshot(&[("a", 12, 11)]))
            .unwrap();
        assert_eq!(increments(next), snapshot(&[("a", 4, 0)]));
    }

    #[test]
    fn snapshot_of_an_older_epoch_is_ignored() {
        let mut tracker = ProductionTracker::default();
        tracker.observe(EPOCH_1, snapshot(&[("a", 26_000, 25_900)]));
        tracker.observe(EPOCH_2, snapshot(&[("a", 8, 7)]));
        assert!(tracker
            .observe(EPOCH_1, snapshot(&[("a", 26_004, 25_904)]))
            .is_none());
        let updates = tracker
            .observe(EPOCH_2, snapshot(&[("a", 12, 11)]))
            .unwrap();
        assert_eq!(increments(updates), snapshot(&[("a", 4, 0)]));
    }

    #[test]
    fn rollover_before_the_first_snapshot_is_still_a_baseline() {
        let mut tracker = ProductionTracker::default();
        let updates = tracker.observe(EPOCH_2, snapshot(&[("a", 8, 7)])).unwrap();
        assert_eq!(increments(updates), snapshot(&[("a", 0, 0)]));
    }
}
