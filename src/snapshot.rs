//! The one data shape the Topology tab renders from —
//! fetched entirely through `embarch-core-client` (decision 5's amendment:
//! no in-process hardware access at all). A
//! background task (`main.rs`) polls Core on an interval and publishes a
//! fresh `Snapshot` on a `tokio::sync::watch` channel; every SSE client and
//! `GET /api/snapshot` call reads the latest published value — the poll
//! against Core happens once per interval regardless of how many browser
//! tabs are open, not once per client.

use embarch_core_client::attest::{Attestation, TokenMode};
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
    /// Carries `confirmed_at_utc_ms` — enrolment time, unmoving until
    /// re-enrolment. Any UI cell built from this field is labeled
    /// "Enrolled" / "Enrolled at", never "Validated", "Last validated" or
    /// "Verified" — those imply a live check this field cannot answer.
    /// `embarch-core` decision 57 declined to
    /// persist a real last-validation instant next to it; real freshness
    /// needs a `POST /validate` call and that response's own
    /// `validated_at_utc_ms`, not a passive read of this snapshot.
    pub enrolled: Vec<EnrolledBoardResponse>,
    pub alerts: Vec<AlertResponse>,
    pub dev_bench_port: Option<DevBenchPortResponse>,
    /// Every declared DUT signal link (`embarch-topology` decision 18) —
    /// the Topology tab's signal-route rows (decision 10).
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
    /// running the UI is not a port on the machine running Core (decision 5).
    pub serial_ports: Vec<SerialPortResponse>,
    pub serial_ports_error: Option<String>,
    /// The one line under the top bar on every tab: which Core this is
    /// (decision 112).
    pub core_identity: CoreIdentity,
}

/// Which Core answers, as the line every tab shows. The words are built here,
/// once, so the page only draws them and a test can read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoreIdentity {
    pub text: String,
    /// Drawn as a warning: the Core is not the installed one, or could not be checked.
    pub warning: bool,
}

/// Hex characters of the fingerprint the line shows.
const FINGERPRINT_SHOWN: usize = 16;

impl CoreIdentity {
    /// Shown until the first poll answers.
    pub fn pending() -> CoreIdentity {
        CoreIdentity { text: "Core: identity not read yet".to_string(), warning: false }
    }

    /// The line for what the client's `attestation()` reported.
    ///
    /// `service` or `development` is read from whether the Core's key is the
    /// installed one: `embarch-core-client`'s `StatusResponse` drops the
    /// `mode` Core reports, so the UI cannot read that field itself. A Core
    /// that gave no fingerprint says its mode is unknown.
    pub fn from_attestation(a: &Attestation) -> CoreIdentity {
        let Some(fp) = a.fingerprint.as_deref() else {
            return match (&a.error, a.mode) {
                (Some(e), _) => CoreIdentity {
                    text: format!("Core: did not report a mode or fingerprint ({e})"),
                    warning: true,
                },
                (None, TokenMode::Unattested) => CoreIdentity {
                    text: "Core: mode and fingerprint not reported, no attested install on this machine".to_string(),
                    warning: false,
                },
                (None, _) => CoreIdentity { text: "Core: did not report a mode or fingerprint".to_string(), warning: true },
            };
        };
        let shown: String = fp.chars().take(FINGERPRINT_SHOWN).collect();
        let (mode, verdict, warning) = match a.matches {
            Some(true) => ("service", "matches the installed fingerprint", false),
            Some(false) => ("development", "does not match", true),
            None => ("mode unknown", "no attested install on this machine", false),
        };
        CoreIdentity { text: format!("Core: {mode}, {shown}, {verdict}"), warning }
    }
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
            core_identity: CoreIdentity::pending(),
        }
    }
}

/// One round of the background poll. Each call fails independently rather
/// than short-circuiting the others — Core being unreachable is an expected,
/// renderable state (decision 26: Core down is not a crash), not something
/// that should leave most of the snapshot silently empty when only one call
/// actually failed.
///
/// The two signal calls carry their errors instead of degrading to an empty
/// list, unlike the four above them. The difference is what an empty answer
/// would *mean*: no enrolled boards and no alerts are real states a human
/// reads correctly, while "no signals declared" against a Core that does not
/// have the endpoint is a false statement about the bench.
pub async fn poll(core: &CoreClient) -> Snapshot {
    let (status, enrolled, alerts, dev_bench_port, signals, serial_ports, attestation) = tokio::join!(
        core.status(),
        core.list_enrolled(),
        core.alerts(20),
        core.dev_bench_port(),
        core.list_signals(),
        core.list_serial_ports(),
        core.attestation(),
    );

    let core_reachable = status.is_ok();
    // `status`'s own error is the most representative "why is Core
    // unreachable" message — if it failed, the other five almost
    // certainly failed for the same underlying reason (wrong base_url, no
    // process listening, a bad token), so surfacing one clear cause beats
    // concatenating five near-identical ones.
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
        core_identity: CoreIdentity::from_attestation(&attestation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const DEV_FP: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn att(mode: TokenMode, fp: Option<&str>, installed: Option<&str>, matches: Option<bool>, error: Option<&str>) -> Attestation {
        Attestation {
            mode,
            fingerprint: fp.map(str::to_string),
            installed_fingerprint: installed.map(str::to_string),
            matches,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn a_service_core_reads_service_with_sixteen_hex_and_a_match() {
        let id = CoreIdentity::from_attestation(&att(TokenMode::Matched, Some(SERVICE_FP), Some(SERVICE_FP), Some(true), None));
        assert_eq!(id.text, "Core: service, 0123456789abcdef, matches the installed fingerprint");
        assert!(!id.warning);
    }

    #[test]
    fn a_development_core_reads_development_and_is_a_warning() {
        let id = CoreIdentity::from_attestation(&att(TokenMode::Development, Some(DEV_FP), Some(SERVICE_FP), Some(false), None));
        assert_eq!(id.text, "Core: development, fedcba9876543210, does not match");
        assert!(id.warning);
    }

    #[test]
    fn a_refused_foreign_core_is_still_a_mismatch_warning() {
        let id = CoreIdentity::from_attestation(&att(TokenMode::Refused, Some(DEV_FP), Some(SERVICE_FP), Some(false), Some("no development token")));
        assert!(id.warning);
        assert!(id.text.contains("does not match"), "{}", id.text);
    }

    #[test]
    fn no_installed_fingerprint_says_no_attested_install_and_is_not_a_warning() {
        let id = CoreIdentity::from_attestation(&att(TokenMode::Unattested, None, None, None, None));
        assert!(id.text.contains("no attested install on this machine"), "{}", id.text);
        assert!(!id.warning);
        let id = CoreIdentity::from_attestation(&att(TokenMode::Explicit, Some(SERVICE_FP), None, None, None));
        assert!(id.text.contains("no attested install on this machine"), "{}", id.text);
    }

    #[test]
    fn a_core_that_reports_no_mode_gets_a_line_saying_so() {
        let id = CoreIdentity::from_attestation(&att(TokenMode::Refused, None, Some(SERVICE_FP), None, Some("404 on /auth/challenge")));
        assert!(id.text.contains("did not report a mode or fingerprint"), "{}", id.text);
        assert!(id.text.contains("404"), "{}", id.text);
        assert!(id.warning);
    }
}
