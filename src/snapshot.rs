//! The one data shape both the Dashboard and Topology tabs render from —
//! fetched entirely through `embarch-core-client` (embarch-ui/design.md §3
//! decision 5's amendment: no in-process hardware access at all). A
//! background task (`main.rs`) polls Core on an interval and publishes a
//! fresh `Snapshot` on a `tokio::sync::watch` channel; every SSE client and
//! `GET /api/snapshot` call reads the latest published value — the poll
//! against Core happens once per interval regardless of how many browser
//! tabs are open, not once per client.

use embarch_core_client::{AlertResponse, CoreClient, DevBenchPortResponse, EnrolledBoardResponse, ProbeInfo};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub core_reachable: bool,
    pub error: Option<String>,
    pub probes: Vec<ProbeInfo>,
    pub enrolled: Vec<EnrolledBoardResponse>,
    pub alerts: Vec<AlertResponse>,
    pub dev_bench_port: Option<DevBenchPortResponse>,
}

impl Snapshot {
    /// Shown until the first real poll completes.
    pub fn pending() -> Snapshot {
        Snapshot {
            core_reachable: false,
            error: Some("waiting for the first poll of embarch-core…".to_string()),
            probes: Vec::new(),
            enrolled: Vec::new(),
            alerts: Vec::new(),
            dev_bench_port: None,
        }
    }
}

/// One round of the background poll. Each of the four calls fails
/// independently rather than short-circuiting the others — Core being
/// unreachable is an expected, renderable state (§ design.md decision 5's
/// own "confirmed" reasoning: Core down is not a crash), not something that
/// should leave three-quarters of the snapshot silently empty when only one
/// call actually failed.
pub async fn poll(core: &CoreClient) -> Snapshot {
    let (status, enrolled, alerts, dev_bench_port) = tokio::join!(
        core.status(),
        core.list_enrolled(),
        core.alerts(20),
        core.dev_bench_port(),
    );

    let core_reachable = status.is_ok();
    // `status`'s own error is the most representative "why is Core
    // unreachable" message — if it failed, the other three almost
    // certainly failed for the same underlying reason (wrong base_url, no
    // process listening, a bad token), so surfacing one clear cause beats
    // concatenating four near-identical ones.
    let error = status.as_ref().err().map(|e| format!("{e:#}"));

    Snapshot {
        core_reachable,
        error,
        probes: status.map(|s| s.probes).unwrap_or_default(),
        enrolled: enrolled.unwrap_or_default(),
        alerts: alerts.unwrap_or_default(),
        dev_bench_port: dev_bench_port.unwrap_or_default(),
    }
}
