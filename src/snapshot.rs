//! The one data shape both the Dashboard and Topology tabs render from —
//! fetched entirely through `embarch-core-client` (embarch-ui/design.md §3
//! decision 5's amendment: no in-process hardware access at all). A
//! background task (`main.rs`) polls Core on an interval and publishes a
//! fresh `Snapshot` on a `tokio::sync::watch` channel; every SSE client and
//! `GET /api/snapshot` call reads the latest published value — the poll
//! against Core happens once per interval regardless of how many browser
//! tabs are open, not once per client.

use embarch_core_client::{
    AlertResponse, CoreClient, DevBenchPortResponse, EnrolledBoardResponse, ProbeInfo,
    SerialPortResponse, SignalLink,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub core_reachable: bool,
    pub error: Option<String>,
    pub probes: Vec<ProbeInfo>,
    pub enrolled: Vec<EnrolledBoardResponse>,
    pub alerts: Vec<AlertResponse>,
    pub dev_bench_port: Option<DevBenchPortResponse>,
    /// Every declared DUT signal link (`embarch-topology/design.md` §3
    /// decision 18) — the Topology tab's signal-route rows
    /// (`embarch-ui/design.md` §3 decision 10).
    pub signals: Vec<SignalLink>,
    /// Why the signal list is empty, when the reason is not "nothing is
    /// declared".
    ///
    /// **This field is load-bearing and its absence would have been a real
    /// lie.** A Core older than `POST`/`GET /signals` answers `404` on both,
    /// and folding that into an empty list would render "no signals declared"
    /// against a Core that has no idea what a signal is. That is not
    /// hypothetical: this bench's own live Core is exactly that Core as of
    /// 2026-08-26, so the very first thing the tab shows a human would have
    /// been wrong. Same reasoning for [`Snapshot::serial_ports_error`].
    pub signals_error: Option<String>,
    /// Core's own serial-port enumeration — what a `direct` route's carrier is
    /// picked from. **Core's**, not this process's: a port on the machine
    /// running the UI is not a port on the machine running Core (design.md §3
    /// decision 5).
    pub serial_ports: Vec<SerialPortResponse>,
    pub serial_ports_error: Option<String>,
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
            signals: Vec::new(),
            signals_error: None,
            serial_ports: Vec::new(),
            serial_ports_error: None,
        }
    }
}

/// One round of the background poll. Each call fails independently rather
/// than short-circuiting the others — Core being unreachable is an expected,
/// renderable state (§ design.md decision 5's own "confirmed" reasoning: Core
/// down is not a crash), not something that should leave most of the snapshot
/// silently empty when only one call actually failed.
///
/// The two signal calls carry their errors instead of degrading to an empty
/// list, unlike the four above them. The difference is what an empty answer
/// would *mean*: no enrolled boards and no alerts are real states a human
/// reads correctly, while "no signals declared" against a Core that does not
/// have the endpoint is a false statement about the bench.
pub async fn poll(core: &CoreClient) -> Snapshot {
    let (status, enrolled, alerts, dev_bench_port, signals, serial_ports) = tokio::join!(
        core.status(),
        core.list_enrolled(),
        core.alerts(20),
        core.dev_bench_port(),
        core.list_signals(),
        core.list_serial_ports(),
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
        // Only worth reporting when Core itself answered: if Core is
        // unreachable the banner above already says so, and repeating it once
        // per sub-call would bury the one message that matters.
        signals_error: match (&signals, core_reachable) {
            (Err(e), true) => Some(format!("{e:#}")),
            _ => None,
        },
        signals: signals.unwrap_or_default(),
        serial_ports_error: match (&serial_ports, core_reachable) {
            (Err(e), true) => Some(format!("{e:#}")),
            _ => None,
        },
        serial_ports: serial_ports.unwrap_or_default(),
    }
}
