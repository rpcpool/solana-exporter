//! Offloaded leader-enrichment sources for the joinable `solana_validator_node_info`
//! metric: ip-api geolocation (country + ASN) and DoubleZero edge membership.
//!
//! Both are refreshed by a single background task so the per-cycle metric publish
//! never blocks on an external API. The caches are sticky: a transient API failure
//! leaves the last-known value in place rather than downgrading it to "unknown"
//! (which would churn Prometheus series). An IP is only reported as unknown until
//! its first successful lookup, then retried on the next tick.

use anyhow::Context;
use log::{debug, warn};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Default ip-api base URL (free tier, HTTP-only).
pub const DEFAULT_IP_API_BASE_URL: &str = "http://ip-api.com";
/// Default DoubleZero publisher-check endpoint.
pub const DEFAULT_DZ_PUBLISHER_URL: &str = "https://data.malbeclabs.com/api/dz/publisher-check";
/// ip-api batches accept at most 100 queries per request.
const IP_API_BATCH_SIZE: usize = 100;
/// Spacing between batch requests: one per 1.5s caps the rate at ~40/min, under
/// the free tier's ~45/min limit while continuously draining the backlog.
const IP_API_BATCH_SPACING: Duration = Duration::from_millis(1500);
/// Idle poll interval once every wanted IP is resolved or backing off. Short so
/// the initial drain starts promptly after the main loop publishes its first
/// target set; each idle poll is just an in-memory read.
const IDLE_INTERVAL: Duration = Duration::from_secs(5);
/// Backoff after a whole-batch failure (network error / rate limit) before
/// retrying, so an ip-api outage does not spin.
const ERROR_BACKOFF: Duration = Duration::from_secs(10);
/// How long to wait before re-querying an IP whose lookup returned no location
/// (e.g. a private or unlocatable address), so we do not burn the rate budget
/// retrying permanent failures every pass.
const NEGATIVE_BACKOFF: Duration = Duration::from_secs(30 * 60);

/// Parses the bare IP out of an optional `ip:port` (or bracketed IPv6) string.
pub fn socket_ip(addr: &Option<String>) -> Option<IpAddr> {
    let value = addr.as_deref()?;
    if let Ok(socket) = value.parse::<SocketAddr>() {
        return Some(socket.ip());
    }
    value
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(value)
        .trim_matches(|c| c == '[' || c == ']')
        .parse()
        .ok()
}

/// Geolocation for a single IP. `country_code` present means "resolved".
#[derive(Clone, Debug, Default)]
pub struct IpGeo {
    pub country_code: Option<String>,
    pub asn: Option<u32>,
}

/// Cache entry: a resolved geo (`country_code.is_some()`) is sticky forever; an
/// unresolved entry carries the time after which it may be retried.
#[derive(Clone, Default)]
struct GeoEntry {
    geo: IpGeo,
    retry_after: Option<Instant>,
}

/// Sticky in-memory IP -> geolocation cache shared between the background
/// refresh task and the metric-publish path.
#[derive(Clone, Default)]
pub struct IpGeoCache {
    inner: Arc<RwLock<HashMap<IpAddr, GeoEntry>>>,
}

impl IpGeoCache {
    /// Resolved geolocation for `ip`, or `None` while unknown.
    pub fn get(&self, ip: &IpAddr) -> Option<IpGeo> {
        let map = self.inner.read().ok()?;
        let entry = map.get(ip)?;
        entry.geo.country_code.as_ref().map(|_| entry.geo.clone())
    }

    /// Global IPs that still need a lookup: never queried, or a prior failure
    /// whose backoff has elapsed. Non-global (private/loopback) IPs are skipped
    /// entirely since ip-api cannot locate them.
    fn due_for_lookup(&self, ips: &[IpAddr]) -> Vec<IpAddr> {
        let now = Instant::now();
        let guard = self.inner.read();
        ips.iter()
            .copied()
            .filter(is_global)
            .filter(|ip| match guard.as_ref().ok().and_then(|map| map.get(ip)) {
                None => true,
                Some(entry) => {
                    entry.geo.country_code.is_none()
                        && entry.retry_after.map(|at| at <= now).unwrap_or(true)
                }
            })
            .collect()
    }

    /// Records freshly resolved locations (sticky).
    fn mark_resolved(&self, records: impl IntoIterator<Item = (IpAddr, IpGeo)>) {
        if let Ok(mut map) = self.inner.write() {
            for (ip, geo) in records {
                map.insert(
                    ip,
                    GeoEntry {
                        geo,
                        retry_after: None,
                    },
                );
            }
        }
    }

    /// Records a lookup that returned no location, scheduling a retry after the
    /// negative backoff. Never clobbers an already-resolved entry.
    fn mark_failed(&self, ips: impl IntoIterator<Item = IpAddr>) {
        let retry_after = Some(Instant::now() + NEGATIVE_BACKOFF);
        if let Ok(mut map) = self.inner.write() {
            for ip in ips {
                let entry = map.entry(ip).or_default();
                if entry.geo.country_code.is_none() {
                    entry.retry_after = retry_after;
                }
            }
        }
    }
}

/// Whether an IP is globally routable enough to bother geolocating. Skips
/// loopback, private, link-local, broadcast, documentation and unspecified
/// ranges (e.g. our own WireGuard gossip addresses).
fn is_global(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified())
        }
        IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unspecified()),
    }
}

/// DoubleZero edge publishers for the current epoch. `None` means never fetched.
#[derive(Clone, Default)]
pub struct DzEdgeCache {
    inner: Arc<RwLock<Option<DzEdgeState>>>,
}

struct DzEdgeState {
    epoch: u64,
    edges: HashSet<String>,
}

impl DzEdgeCache {
    /// `Some(true|false)` once fetched, `None` while unknown.
    pub fn is_edge(&self, node_pubkey: &str) -> Option<bool> {
        let guard = self.inner.read().ok()?;
        let state = guard.as_ref()?;
        Some(state.edges.contains(node_pubkey))
    }

    fn cached_epoch(&self) -> Option<u64> {
        self.inner
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|state| state.epoch))
    }

    fn install(&self, epoch: u64, edges: HashSet<String>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = Some(DzEdgeState { epoch, edges });
        }
    }
}

/// Shared handles the main loop uses to feed the background task and read the
/// enrichment caches when publishing metrics.
#[derive(Clone)]
pub struct Enrichment {
    pub geo: IpGeoCache,
    pub dz: DzEdgeCache,
    wanted_ips: Arc<RwLock<Vec<IpAddr>>>,
    current_epoch: Arc<AtomicU64>,
}

impl Enrichment {
    pub fn new() -> Self {
        Self {
            geo: IpGeoCache::default(),
            dz: DzEdgeCache::default(),
            wanted_ips: Arc::new(RwLock::new(Vec::new())),
            current_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Publishes the current cluster gossip IPs and epoch for the background task
    /// to work from. Called once per cycle from the main loop.
    pub fn set_targets(&self, ips: Vec<IpAddr>, epoch: u64) {
        if let Ok(mut guard) = self.wanted_ips.write() {
            *guard = ips;
        }
        self.current_epoch.store(epoch, Ordering::Relaxed);
    }

    /// Spawns the background refresh task. `dz_url` = `None` disables DZ edge.
    pub fn spawn_task(&self, ip_api_base_url: String, dz_url: Option<String>) {
        let geo = self.geo.clone();
        let dz = self.dz.clone();
        let wanted_ips = Arc::clone(&self.wanted_ips);
        let current_epoch = Arc::clone(&self.current_epoch);
        let ip_api = IpApiClient::new(reqwest::Client::new(), ip_api_base_url);
        let dz_client = reqwest::Client::new();

        // Continuous, self-pacing drain: one ip-api batch per tick (~40/min),
        // idling when everything is resolved or backing off. DZ edge is refreshed
        // opportunistically each pass (a cheap per-epoch gate).
        tokio::spawn(async move {
            loop {
                if let Some(url) = dz_url.as_deref() {
                    refresh_dz(&dz_client, &dz, &current_epoch, url).await;
                }

                let ips = wanted_ips
                    .read()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();
                let mut due = geo.due_for_lookup(&ips);
                if due.is_empty() {
                    tokio::time::sleep(IDLE_INTERVAL).await;
                    continue;
                }

                due.truncate(IP_API_BATCH_SIZE);
                match ip_api.lookup_batch(&due).await {
                    Ok(found) => {
                        let resolved: HashSet<IpAddr> = found.iter().map(|(ip, _)| *ip).collect();
                        let failed = due.iter().copied().filter(|ip| !resolved.contains(ip));
                        geo.mark_failed(failed);
                        geo.mark_resolved(found);
                        tokio::time::sleep(IP_API_BATCH_SPACING).await;
                    }
                    Err(error) => {
                        // Whole-batch failure (network / rate limit): keep last-known
                        // and back off without penalizing the individual IPs.
                        warn!("ip-api batch failed: {error:#}");
                        tokio::time::sleep(ERROR_BACKOFF).await;
                    }
                }
            }
        });
    }
}

impl Default for Enrichment {
    fn default() -> Self {
        Self::new()
    }
}

async fn refresh_dz(
    client: &reqwest::Client,
    dz: &DzEdgeCache,
    current_epoch: &Arc<AtomicU64>,
    url: &str,
) {
    let epoch = current_epoch.load(Ordering::Relaxed);
    // epoch 0 = the main loop has not published one yet; skip until it has.
    if epoch == 0 || dz.cached_epoch() == Some(epoch) {
        return;
    }
    match fetch_dz_edges(client, url).await {
        Ok(edges) => {
            debug!("DZ edge: {} publisher(s) @ epoch {}", edges.len(), epoch);
            dz.install(epoch, edges);
        }
        Err(error) => warn!("DZ edge refresh failed: {error:#}"),
    }
}

struct IpApiClient {
    client: reqwest::Client,
    base_url: String,
}

#[derive(Deserialize)]
struct IpApiRecord {
    status: String,
    #[serde(rename = "countryCode")]
    country_code: Option<String>,
    query: Option<String>,
    #[serde(rename = "as")]
    as_name: Option<String>,
}

impl IpApiClient {
    fn new(client: reqwest::Client, base_url: String) -> Self {
        Self { client, base_url }
    }

    async fn lookup_batch(&self, ips: &[IpAddr]) -> anyhow::Result<Vec<(IpAddr, IpGeo)>> {
        if ips.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!(
            "{}/batch?fields=status,countryCode,query,as",
            self.base_url.trim_end_matches('/')
        );
        let body: Vec<_> = ips
            .iter()
            .map(|ip| serde_json::json!({ "query": ip.to_string() }))
            .collect();

        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .context("ip-api batch request failed")?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("ip-api batch returned HTTP {status}: {text}");
        }
        let records: Vec<IpApiRecord> = response
            .json()
            .await
            .context("failed to decode ip-api batch response")?;
        Ok(records.into_iter().filter_map(record_to_geo).collect())
    }
}

fn record_to_geo(record: IpApiRecord) -> Option<(IpAddr, IpGeo)> {
    if record.status != "success" {
        return None;
    }
    let ip: IpAddr = record.query?.parse().ok()?;
    let country_code = record
        .country_code
        .filter(|code| !code.trim().is_empty())
        .map(|code| code.to_ascii_uppercase())?;
    let asn = record.as_name.as_deref().and_then(parse_asn);
    Some((
        ip,
        IpGeo {
            country_code: Some(country_code),
            asn,
        },
    ))
}

/// Extracts the ASN number (e.g. 16509) from an ip-api `as` string like
/// "AS16509 Amazon.com, Inc.".
fn parse_asn(as_name: &str) -> Option<u32> {
    let trimmed = as_name.trim();
    let stripped = trimmed.strip_prefix("AS").unwrap_or(trimmed);
    let head: String = stripped.chars().take_while(|c| c.is_ascii_digit()).collect();
    head.parse().ok()
}

#[derive(Deserialize)]
struct DzResponse {
    publishers: Vec<DzPublisher>,
}

#[derive(Deserialize)]
struct DzPublisher {
    node_pubkey: String,
    #[serde(default)]
    publishing_leader_shreds: bool,
}

async fn fetch_dz_edges(client: &reqwest::Client, url: &str) -> anyhow::Result<HashSet<String>> {
    let response = client
        .get(url)
        .send()
        .await
        .context("DZ publisher request failed")?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("DZ publisher returned HTTP {status}: {text}");
    }
    let parsed: DzResponse = response
        .json()
        .await
        .context("failed to decode DZ publisher response")?;
    Ok(parsed
        .publishers
        .into_iter()
        .filter(|publisher| publisher.publishing_leader_shreds)
        .map(|publisher| publisher.node_pubkey)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{parse_asn, socket_ip};

    #[test]
    fn parses_asn() {
        assert_eq!(parse_asn("AS16509 Amazon.com, Inc."), Some(16509));
        assert_eq!(parse_asn("16509"), Some(16509));
        assert_eq!(parse_asn("Amazon"), None);
    }

    #[test]
    fn parses_socket_ip() {
        assert_eq!(
            socket_ip(&Some("64.130.42.197:8002".to_string())).map(|ip| ip.to_string()),
            Some("64.130.42.197".to_string())
        );
        assert_eq!(
            socket_ip(&Some("[2001:db8::1]:8001".to_string())).map(|ip| ip.to_string()),
            Some("2001:db8::1".to_string())
        );
        assert_eq!(socket_ip(&None), None);
    }
}
