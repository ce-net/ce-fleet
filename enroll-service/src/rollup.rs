//! Fleet rollup — aggregate the mesh view (`atlas` + per-node `status`/`history`) across the
//! delegate's subtree into one `/fleet/rollup` payload, so the admin console hits a few delegates
//! instead of opening 1500 SSE streams.
//!
//! The aggregation is pure and fetcher-injected: `RollupAggregator` takes any source implementing
//! [`MeshSource`] (the live one wraps `ce_rs::CeClient`; tests pass a canned one), builds one
//! [`NodeView`] per atlas entry, and derives fleet-wide rollup counters. No node changes — every
//! input is an existing ce-rs read.

use std::future::Future;

use anyhow::Result;
use ce_rs::{AtlasEntry, NodeHistory};
use serde::{Deserialize, Serialize};

/// The role we infer for a node from its atlas self-tags and current load. Purely a display
/// classification for the swarm grid (the real authority is always the capability chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Advertises `infer` and is serving at least one job.
    Worker,
    /// Advertises `infer` but is currently idle.
    Idle,
    /// Meshed but not an inference worker (no `infer` tag) — e.g. a pure relay/router seed.
    Router,
    /// In the atlas but its last-seen is older than the freshness window — treated as offline.
    Offline,
}

/// One node as the console renders it: identity + health + role + the capability/tier facts the
/// admin grid needs. Built from one atlas entry plus an optional history lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub node_id: String,
    pub cpu_cores: u32,
    pub mem_mb: u32,
    pub running_jobs: u32,
    pub last_seen_secs: u64,
    pub tags: Vec<String>,
    /// `GpuHeavy`/`CpuLow`/… parsed from a `tier:<x>` self-tag, if present.
    pub tier: Option<String>,
    /// `model:<id>` self-tag, if present (the model this worker serves).
    pub model: Option<String>,
    pub role: Role,
    /// `delivered_work()` from `/history`, when the source supplied it (reputation substrate).
    pub delivered_work: Option<u64>,
}

/// Fleet-wide counters the console headline shows (online/worker counts, the enrollment funnel's
/// "live" number). Derived from the per-node views.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubtreeRollup {
    pub total: usize,
    pub workers: usize,
    pub idle: usize,
    pub routers: usize,
    pub offline: usize,
    /// Sum of running inference jobs across live workers.
    pub running_jobs: u64,
    pub nodes: Vec<NodeView>,
}

/// The reads the aggregator needs from the mesh. The live impl wraps `ce_rs::CeClient`; tests pass
/// a fake. History is optional per node (the aggregator tolerates a missing/failed lookup).
pub trait MeshSource {
    /// The full atlas this delegate can see (its subtree).
    fn atlas(&self) -> impl Future<Output = Result<Vec<AtlasEntry>>> + Send;
    /// Best-effort history for one node (reputation). `Ok(None)` if unavailable.
    fn history(&self, node_id: &str) -> impl Future<Output = Result<Option<NodeHistory>>> + Send;
}

/// Builds rollups from a [`MeshSource`]. Stateless beyond the source + the freshness window.
pub struct RollupAggregator<S: MeshSource> {
    source: S,
    /// A node whose `last_seen_secs` exceeds this is rendered `Offline`. Atlas self-tags refresh
    /// ~every 60s, so a window of a few multiples of that avoids flapping during enroll waves.
    fresh_window_secs: u64,
}

impl<S: MeshSource> RollupAggregator<S> {
    pub fn new(source: S, fresh_window_secs: u64) -> Self {
        Self {
            source,
            fresh_window_secs,
        }
    }

    /// Build the full subtree rollup. Includes a best-effort history lookup per node; a node whose
    /// history fetch errors simply has `delivered_work: None` (the grid still renders it).
    pub async fn rollup(&self) -> Result<SubtreeRollup> {
        let atlas = self.source.atlas().await?;
        let mut out = SubtreeRollup::default();
        for entry in atlas {
            let delivered = match self.source.history(&entry.node_id).await {
                Ok(h) => h.map(|h| h.delivered_work()),
                Err(_) => None, // tolerate a flaky/stale history read; the node still shows up
            };
            let view = self.classify(&entry, delivered);
            match view.role {
                Role::Worker => {
                    out.workers += 1;
                    out.running_jobs += u64::from(view.running_jobs);
                }
                Role::Idle => out.idle += 1,
                Role::Router => out.routers += 1,
                Role::Offline => out.offline += 1,
            }
            out.total += 1;
            out.nodes.push(view);
        }
        Ok(out)
    }

    /// Classify one atlas entry into a [`NodeView`] (pure — the unit-tested core).
    fn classify(&self, e: &AtlasEntry, delivered_work: Option<u64>) -> NodeView {
        let tier = e
            .tags
            .iter()
            .find_map(|t| t.strip_prefix("tier:").map(|s| s.to_string()));
        let model = e
            .tags
            .iter()
            .find_map(|t| t.strip_prefix("model:").map(|s| s.to_string()));
        let is_infer = e.has_tag("infer");
        let role = if e.last_seen_secs > self.fresh_window_secs {
            Role::Offline
        } else if is_infer && e.running_jobs > 0 {
            Role::Worker
        } else if is_infer {
            Role::Idle
        } else {
            Role::Router
        };
        NodeView {
            node_id: e.node_id.clone(),
            cpu_cores: e.cpu_cores,
            mem_mb: e.mem_mb,
            running_jobs: e.running_jobs,
            last_seen_secs: e.last_seen_secs,
            tags: e.tags.clone(),
            tier,
            model,
            role,
            delivered_work,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSource {
        atlas: Vec<AtlasEntry>,
    }

    impl MeshSource for FakeSource {
        async fn atlas(&self) -> Result<Vec<AtlasEntry>> {
            Ok(self.atlas.clone())
        }
        async fn history(&self, _node_id: &str) -> Result<Option<NodeHistory>> {
            Ok(None)
        }
    }

    fn entry(id: &str, tags: &[&str], jobs: u32, last_seen: u64) -> AtlasEntry {
        // AtlasEntry is `#[derive(Deserialize)]` with no public ctor; build it via JSON so we use
        // only its public shape (and stay correct if fields are added with serde defaults).
        let json = serde_json::json!({
            "node_id": id,
            "cpu_cores": 8,
            "mem_mb": 16000,
            "running_jobs": jobs,
            "last_seen_secs": last_seen,
            "tags": tags,
        });
        serde_json::from_value(json).expect("atlas entry")
    }

    #[tokio::test]
    async fn classifies_worker_idle_router_offline() {
        let src = FakeSource {
            atlas: vec![
                entry("a", &["infer", "tier:GpuMid", "model:clinical-chat-8b"], 3, 10),
                entry("b", &["infer", "tier:CpuLow"], 0, 20),
                entry("c", &["relay"], 0, 5),
                entry("d", &["infer"], 1, 9_999), // stale → offline despite running a job
            ],
        };
        let agg = RollupAggregator::new(src, 180);
        let r = agg.rollup().await.expect("rollup");
        assert_eq!(r.total, 4);
        assert_eq!(r.workers, 1);
        assert_eq!(r.idle, 1);
        assert_eq!(r.routers, 1);
        assert_eq!(r.offline, 1);
        assert_eq!(r.running_jobs, 3);

        let a = r.nodes.iter().find(|n| n.node_id == "a").expect("a");
        assert_eq!(a.role, Role::Worker);
        assert_eq!(a.tier.as_deref(), Some("GpuMid"));
        assert_eq!(a.model.as_deref(), Some("clinical-chat-8b"));
    }
}
