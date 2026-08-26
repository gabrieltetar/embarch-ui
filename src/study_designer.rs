//! The Study Designer tab's backend (milestone-1.md §4.6): merged action
//! list, custom-action registry, and build/run/watch — all in-process
//! authoring via `embarch-study-designer` (pure/offline, no hardware
//! touched), submission/execution via `embarch-core-client` over HTTP+Bearer
//! (design.md §3 decision 5). Unlike `study-designer-ui`, which shells out
//! to `embarch-api`'s CLI for `run-study`/`study-status`, this talks to
//! `embarch-core` directly through the same shared client the Dashboard/
//! Topology/Enroll tabs already use.
//!
//! Disabled entirely (every route below answers `404`) when `[study_designer]`
//! isn't set in config — this session resolved via `AskUserQuestion` that a
//! config field, not a UI picker or cwd search, names the firmware repo.

use crate::config::StudyDesignerConfig;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use embarch_core_client::{CoreClient, StudyRunOptions};
use embarch_study_designer::limits::{
    MAX_FIRMWARE_VERSION_LEN, MAX_SIGNAL_NAME_LEN, MAX_STREAMS_PER_STUDY, MAX_STREAM_NAME_LEN,
};
use embarch_study_designer::{
    build_study, merge_actions, requirement_satisfied, validate_taps, ActionRegistry,
    BuiltInActionKind, ZephyrBleDefExtractor, GattConfigExtractor, GattServiceInfo, Provenance,
    RegisteredAction, Requirements, RoleChoice, RowAction, StreamEncoding, StreamScope,
    StreamSource, StreamTap, Study, StudyResult, TableRow, VersionSource, REQUIREMENT_ANY,
};
use heapless::String as HString;
use heapless::Vec as HVec;

/// `Study.streams`' own type, named once rather than spelled out at each use.
type StreamList = HVec<StreamTap, MAX_STREAMS_PER_STUDY>;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::watch;

/// How long `POST /api/study-designer/discover` waits for a one-step
/// `BleConnect`->`GattDiscover` study to reach a terminal state before
/// giving up — matches `study-designer-ui`'s own precedent
/// (`embarch-study-designer/milestone-11.md` §3.6).
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

struct Inner {
    config: StudyDesignerConfig,
    core: Arc<CoreClient>,
    /// The most recent live `GattDiscover` result, if any — `None` until
    /// `POST /api/study-designer/discover` has succeeded at least once.
    live_gatt: Mutex<Option<Vec<GattServiceInfo>>>,
    /// Computed once, lazily, on first need — the firmware repo's source
    /// tree doesn't change while this process is running.
    static_gatt: OnceLock<Option<Vec<GattServiceInfo>>>,
    run_tx: watch::Sender<RunState>,
}

#[derive(Clone)]
pub struct StudyDesigner(Arc<Inner>);

impl StudyDesigner {
    pub fn new(config: StudyDesignerConfig, core: Arc<CoreClient>) -> StudyDesigner {
        let (run_tx, _) = watch::channel(RunState::Idle);
        StudyDesigner(Arc::new(Inner {
            config,
            core,
            live_gatt: Mutex::new(None),
            static_gatt: OnceLock::new(),
            run_tx,
        }))
    }

    fn registry(&self) -> Result<ActionRegistry, String> {
        ActionRegistry::load(&self.0.config.firmware_repo_path).map_err(|e| e.to_string())
    }

    /// Runs the configured `static_extractor` at most once per process.
    /// An unrecognized name is a named error the first time it's needed,
    /// not a silent guess — `reference-dut` is the only name this crate
    /// currently ships an extractor for (design.md §3 decision 33).
    fn static_gatt(&self) -> Option<Vec<GattServiceInfo>> {
        self.0
            .static_gatt
            .get_or_init(|| match self.0.config.static_extractor.as_deref() {
                Some("zephyr-ble-def") => ZephyrBleDefExtractor
                    .extract(&self.0.config.firmware_repo_path)
                    .map(|services| services.iter().cloned().collect())
                    .map_err(|e| tracing::warn!("static GATT extraction failed: {e}"))
                    .ok(),
                Some(other) => {
                    tracing::warn!("unrecognized static_extractor '{other}' — only 'reference-dut' exists today");
                    None
                }
                None => None,
            })
            .clone()
    }

    fn live_gatt(&self) -> Option<Vec<GattServiceInfo>> {
        self.0.live_gatt.lock().unwrap().clone()
    }
}

// `StudyResult` is `heapless`-backed with large fixed-capacity buffers
// (`MAX_STEPS_PER_STUDY` steps' worth of `captured_data`/`gatt_services`/
// `gatt_activity`) — over a megabyte inline, the same "oversized stack
// frame" shape `embarch-api/design.md` decision 36 and this crate's own
// design.md §7 already found real stack-overflow risk in. Boxed here so
// `RunState` itself stays small regardless.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Running { study_id: String, current_step: Option<u32>, total_steps: Option<u32> },
    Completed {
        study_id: String,
        result: Box<StudyResult>,
        /// The result's own provenance, flattened with the one judgement the
        /// browser must not make itself: whether each version was *verified*
        /// or merely `Declared` (`VersionSource::is_verified`). Redundant with
        /// `result.provenance` on purpose — rendering `Declared` visibly
        /// weaker is decision 11's requirement, and "the easiest place to
        /// accidentally reintroduce" the defect decision 40 closes is a UI
        /// deciding for itself which variants count.
        provenance: ProvenanceView,
    },
    Failed { study_id: Option<String>, reason: String },
}

fn not_configured() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        "the Study Designer tab needs [study_designer].firmware_repo_path set in embarch-ui's config",
    )
        .into_response()
}

/// A fixed step-count of `MAX_STEPS_PER_STUDY`-independent studies — `Study`
/// name doesn't matter beyond fitting the length limit, so a literal here is
/// fine rather than accepting one from the client for this one-click action.
fn discover_study() -> Result<Study, String> {
    let rows = vec![
        TableRow {
            name: "connect".to_string(),
            action: RowAction::BuiltIn { which: BuiltInActionKind::BleConnect, role: RoleChoice::Central , target_name: None },
            timeout_ms: 15_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        },
        TableRow {
            name: "discover".to_string(),
            action: RowAction::BuiltIn { which: BuiltInActionKind::GattDiscover, role: RoleChoice::Central , target_name: None },
            timeout_ms: 15_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        },
    ];
    // `Requirements::any()`, and said out loud rather than defaulted: this is
    // a fixed one-click read of whatever DUT is in front of the operator, not
    // an authored experiment, so there is no build it could honestly
    // constrain. Every *authored* study now carries what a human stated
    // (`RequirementsInput`).
    build_study(
        "embarch-ui discover",
        RequirementsInput::any().build()?,
        &rows,
        &ActionRegistry::default(),
    )
    .map_err(|e| e.to_string())
}

/// What a study authored in this tab requires
/// (`embarch-study-designer/design.md` §3 decision 40), as the browser stated
/// it — **built 2026-08-26, Milestone 7 Phase D** (`embarch-ui/design.md` §3
/// decision 11). Until then this function returned `Requirements::any()`
/// unconditionally, honestly, because the tab had no fields to say anything
/// else in.
///
/// Both fields are still mandatory and `"any"` is still an explicit legal
/// value — that is the decision's whole point, and the UI expresses it as a
/// checkbox rather than as an empty field that happens to validate, so
/// "I don't care which build" is a thing an operator said rather than a thing
/// they skipped.
///
/// A blank field is refused here rather than quietly turned into `any`:
/// `Requirements::validate` treats blank as the not-thought-about case, and
/// silently upgrading it to a deliberate answer would erase the distinction
/// the whole decision rests on.
#[derive(Debug, Clone, Deserialize)]
pub struct RequirementsInput {
    dev_bench_version: String,
    firmware_version: String,
}

impl RequirementsInput {
    /// `Requirements::any()`, for the one caller that genuinely has no
    /// operator behind it (`discover`, a fixed one-click probe of the DUT's
    /// GATT table that is not an authored study).
    fn any() -> RequirementsInput {
        RequirementsInput {
            dev_bench_version: REQUIREMENT_ANY.to_string(),
            firmware_version: REQUIREMENT_ANY.to_string(),
        }
    }

    fn build(&self) -> Result<Requirements, String> {
        let field = |raw: &str, what: &str| -> Result<HString<MAX_FIRMWARE_VERSION_LEN>, String> {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err(format!(
                    "requires.{what} is blank; state the build this study needs, or tick \
                     \"any build\" if it genuinely doesn't matter"
                ));
            }
            HString::try_from(raw).map_err(|_| {
                format!(
                    "requires.{what} is {} characters and the wire allows {MAX_FIRMWARE_VERSION_LEN}",
                    raw.chars().count()
                )
            })
        };
        let requires = Requirements {
            dev_bench_version: field(&self.dev_bench_version, "dev_bench_version")?,
            firmware_version: field(&self.firmware_version, "firmware_version")?,
        };
        requires.validate().map_err(|e| e.to_string())?;
        Ok(requires)
    }
}

/// One outpost-trace tap, as this tab authors it
/// (`embarch-study-designer/design.md` §3 decision 39,
/// `embarch-outpost/design.md` §3 decisions 11/12).
///
/// **The tap names the signal, never the carrier**, which is why this input
/// has no port and no pins in it: those live in the signal's declared route
/// (Topology tab), so the identical saved study runs unchanged across a
/// rewiring of the bench.
///
/// `OutpostTrace`/`WholeStudy` are not choices offered here. An outpost
/// capture is study-scoped with no live feed by design, and its encoding is
/// the one thing a trace tap can be — offering a menu whose other entries are
/// all wrong would be worse than offering none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapInput {
    /// The output file's name under the study's `streams/` directory, and what
    /// `GET /study/{id}/stream/{name}` takes.
    name: String,
    /// The declared signal this taps (`SignalLink::name`). Core's `POST /study`
    /// pre-flight rejects a tap naming an undeclared signal with a `400`,
    /// which is why the Topology tab's routes come first.
    signal: String,
}

/// Turns authored tap rows into `Study.streams`, sealed by the caller.
///
/// `id` is assigned here as the tap's own index, because that is what `id`
/// *is* — the wire handle every `StreamOpen`/`StreamChunkBatch`/`StreamClose`
/// carries — and `validate_taps` rejects any other value. Nothing about that
/// is a choice for an author to make or get wrong.
fn build_taps(taps: &[TapInput], step_count: usize) -> Result<StreamList, String> {
    let mut out: StreamList = StreamList::new();
    for (index, tap) in taps.iter().enumerate() {
        let name = tap.name.trim();
        let signal = tap.signal.trim();
        if signal.is_empty() {
            return Err(format!("stream tap {} names no signal", index + 1));
        }
        let id = u8::try_from(index)
            .map_err(|_| format!("more than {} stream taps", u8::MAX))?;
        let built = StreamTap {
            id,
            name: HString::try_from(name).map_err(|_| {
                format!("tap name '{name}' is longer than the wire's {MAX_STREAM_NAME_LEN} characters")
            })?,
            source: StreamSource::Signal {
                name: HString::try_from(signal).map_err(|_| {
                    format!(
                        "signal name '{signal}' is longer than the wire's \
                         {MAX_SIGNAL_NAME_LEN} characters"
                    )
                })?,
            },
            encoding: StreamEncoding::OutpostTrace,
            scope: StreamScope::WholeStudy,
        };
        out.push(built).map_err(|_| {
            format!("more than {MAX_STREAMS_PER_STUDY} stream taps in one study")
        })?;
    }
    // The same pre-flight Core runs on submit, run here so an authoring
    // mistake is a message in this tab rather than a `400` from a round trip.
    validate_taps(&out, step_count as u32).map_err(|e| format!("{e:?}"))?;
    Ok(out)
}

/// Every submitter recomputes both of a study's seals immediately before
/// sending (`embarch-study-designer/design.md` §3 decision 26) — the same
/// two lines `embarch-api`'s own `study.rs::reseal_study` uses, inlined here
/// rather than depending on that crate for them.
///
/// `streams_crc` is decision 39's 2026-08-25 amendment's sibling seal over
/// `Study.streams`. This tab authors no taps today, so it always reseals to
/// the empty-list value — which is genuinely 0, not a placeholder — but it
/// is computed rather than assumed, so the day this tab does author one
/// there is nothing to remember to change.
fn seal_crc(study: &mut Study) -> Result<(), String> {
    study.steps_crc = embarch_study_designer::steps_crc(&study.steps).map_err(|e| format!("{e:?}"))?;
    study.streams_crc =
        embarch_study_designer::streams_crc(&study.streams).map_err(|e| format!("{e:?}"))?;
    Ok(())
}

fn first_gatt_services(result: &StudyResult) -> Option<Vec<GattServiceInfo>> {
    result
        .steps
        .iter()
        .find_map(|s| s.gatt_services.as_ref())
        .map(|v| v.iter().cloned().collect())
}

/// Polls `GET /study/{id}` until a terminal status or `timeout` elapses.
/// embarch-ui never consumes Core's own `GET /study/{id}/events` SSE stream
/// directly — polling server-side and republishing over embarch-ui's own
/// SSE (the `run` endpoint's `RunState`, or this function's caller for the
/// synchronous `discover` case) is simpler and gives the same "the browser
/// never polls" property decision 6 asks for.
async fn poll_until_terminal(core: &CoreClient, study_id: &str, timeout: Duration) -> Result<StudyResult, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = core.get_study_status(study_id).await.map_err(|e| format!("{e:#}"))?;
        match status.status.as_str() {
            "completed" => {
                return status
                    .result
                    .ok_or_else(|| "embarch-core reported \"completed\" but returned no result".to_string())
            }
            "failed" => {
                return Err(status.reason.unwrap_or_else(|| "study failed with no reason given".to_string()))
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out after {}s waiting for study {study_id}", timeout.as_secs()));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[derive(Debug, Serialize)]
struct ActionsResponse {
    actions: Vec<embarch_study_designer::MergedAction>,
    live_gatt_available: bool,
    static_gatt_available: bool,
}

/// Shared by the `GET /api/study-designer/actions` handler and `discover`
/// (which returns the freshly-merged list once its own live result lands).
fn actions_response(sd: &StudyDesigner) -> axum::response::Response {
    let registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let live = sd.live_gatt();
    let static_gatt = sd.static_gatt();
    let actions = merge_actions(live.as_deref(), static_gatt.as_deref(), &registry);
    Json(ActionsResponse {
        actions,
        live_gatt_available: live.is_some(),
        static_gatt_available: static_gatt.is_some(),
    })
    .into_response()
}

pub async fn api_actions(State(state): State<crate::AppState>) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    actions_response(&sd)
}

pub async fn api_registry(State(state): State<crate::AppState>) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    match sd.registry() {
        Ok(r) => Json(r).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Upserts one `RegisteredAction` by name — never a semantic "what does
/// this do" field anywhere on this type (`embarch-study-designer/design.md`
/// §3 decision 35's own non-goal).
pub async fn api_register_action(
    State(state): State<crate::AppState>,
    Json(action): Json<RegisteredAction>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let mut registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    registry.actions.retain(|a| a.name != action.name);
    registry.actions.push(action);
    match registry.save(&sd.0.config.firmware_repo_path) {
        Ok(()) => Json(registry).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

pub async fn api_discover(State(state): State<crate::AppState>) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let mut study = match discover_study() {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    if let Err(e) = seal_crc(&mut study) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    // `StudyRunOptions::default()` deliberately, and stated rather than
    // implied: this UI never builds or flashes anything (design.md §3
    // decision 5's amendment routes every hardware-adjacent operation through
    // Core), so it has nothing it could honestly claim to have flashed this
    // run and no standing to wave a version requirement through. Core's gate
    // applies to it exactly as before.
    let study_id = match sd.0.core.post_study(&study, &StudyRunOptions::default()).await {
        Ok(resp) => resp.study_id,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    match poll_until_terminal(&sd.0.core, &study_id, DISCOVER_TIMEOUT).await {
        Ok(result) => {
            let services = first_gatt_services(&result).unwrap_or_default();
            *sd.0.live_gatt.lock().unwrap() = Some(services);
            actions_response(&sd)
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct RunRequest {
    name: String,
    rows: Vec<TableRow>,
    requires: RequirementsInput,
    #[serde(default)]
    taps: Vec<TapInput>,
    /// Proceed past a version requirement this run does not satisfy
    /// (`embarch-study-designer/design.md` §3 decision 40). Ticked in the run
    /// dialog against the actual discrepancy, which decision 11 is explicit
    /// about: the mismatch is shown *before* the run, with both strings, so
    /// the choice is made against the real gap rather than in the abstract.
    ///
    /// Never silently honoured — Core records it in
    /// `StudyResult.provenance.overrides` with both strings, and this tab
    /// renders that.
    #[serde(default)]
    allow_version_mismatch: bool,
}

/// Builds, taps, and seals one authored study — everything `run` and `save`
/// do identically, so a saved file and a submitted study can never disagree
/// about what the rows meant.
fn build_authored(
    req_name: &str,
    rows: &[TableRow],
    requires: &RequirementsInput,
    taps: &[TapInput],
    registry: &ActionRegistry,
) -> Result<Study, (StatusCode, String)> {
    let requires = requires.build().map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let mut study = build_study(req_name, requires, rows, registry)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    study.streams =
        build_taps(taps, rows.len()).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    seal_crc(&mut study).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(study)
}

pub async fn api_run(
    State(state): State<crate::AppState>,
    Json(req): Json<RunRequest>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let study = match build_authored(&req.name, &req.rows, &req.requires, &req.taps, &registry) {
        Ok(s) => s,
        Err((code, e)) => return (code, e).into_response(),
    };
    // `flashed_firmware_version` stays `None`, and that is not an omission:
    // this UI never builds or flashes anything (design.md §3 decision 5's
    // amendment routes every hardware-adjacent operation through Core), so it
    // has nothing it could honestly claim to have put on the DUT. Claiming
    // otherwise is exactly what would turn `VersionSource::FlashedThisRun`
    // from a fact into an assertion.
    let options = StudyRunOptions {
        allow_version_mismatch: req.allow_version_mismatch,
        flashed_firmware_version: None,
    };
    let study_id = match sd.0.core.post_study(&study, &options).await {
        Ok(resp) => resp.study_id,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };

    let _ = sd.0.run_tx.send(RunState::Running { study_id: study_id.clone(), current_step: None, total_steps: None });
    let core = sd.0.core.clone();
    let run_tx = sd.0.run_tx.clone();
    let watched_id = study_id.clone();
    tokio::spawn(async move { watch_study(core, watched_id, run_tx).await });

    Json(serde_json::json!({ "study_id": study_id })).into_response()
}

async fn watch_study(core: Arc<CoreClient>, study_id: String, tx: watch::Sender<RunState>) {
    let start = tokio::time::Instant::now();
    // No hard timeout here, unlike `discover` — a real study can legitimately
    // run far longer than 30s (embarch-study-designer/design.md §3 decision
    // 9's own "unbounded BLE wait" reasoning); it ends when Core reports a
    // terminal status, not on a clock this tab invents.
    loop {
        match core.get_study_status(&study_id).await {
            Ok(status) => match status.status.as_str() {
                "completed" => {
                    let state = match status.result {
                        Some(result) => RunState::Completed {
                            provenance: provenance_view(&result.provenance),
                            study_id,
                            result: Box::new(result),
                        },
                        None => RunState::Failed {
                            study_id: Some(study_id),
                            reason: "embarch-core reported \"completed\" but returned no result".to_string(),
                        },
                    };
                    let _ = tx.send(state);
                    return;
                }
                "failed" => {
                    let _ = tx.send(RunState::Failed {
                        study_id: Some(study_id),
                        reason: status.reason.unwrap_or_else(|| "study failed with no reason given".to_string()),
                    });
                    return;
                }
                _ => {
                    let _ = tx.send(RunState::Running {
                        study_id: study_id.clone(),
                        current_step: status.current_step,
                        total_steps: status.total_steps,
                    });
                }
            },
            Err(e) => {
                let _ = tx.send(RunState::Failed { study_id: Some(study_id), reason: format!("{e:#}") });
                return;
            }
        }
        // A host-side backstop, not a protocol timeout — Core's own
        // watchdog (embarch-study-designer/design.md §3 decision 16) is
        // what actually bounds a hung study; this just stops embarch-ui
        // from polling forever if Core itself never resolves it.
        if start.elapsed() > Duration::from_secs(60 * 30) {
            let _ = tx.send(RunState::Failed {
                study_id: Some(study_id),
                reason: "gave up watching after 30 minutes with no terminal status from embarch-core".to_string(),
            });
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub async fn api_run_events(State(state): State<crate::AppState>) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let rx = sd.0.run_tx.subscribe();
    let stream = futures_util::stream::unfold((rx, true), |(mut rx, first)| async move {
        if !first && rx.changed().await.is_err() {
            return None;
        }
        let run_state = rx.borrow().clone();
        let payload = serde_json::to_string(&run_state).unwrap_or_else(|_| "{}".to_string());
        let event = Event::default().event("run").data(payload);
        Some((Ok::<Event, Infallible>(event), (rx, false)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

// ---- saved study library (embarch-study-designer/design.md §3 decision 38) --
//
// One file per saved study at `<firmware-repo>/embarch/studies/<slug>.json`,
// sibling to the `study-actions.toml` registry — same per-repo convention,
// so a study travels with the firmware it was written against.
//
// The file *is* a `Study`, so `embarch-api run-study --study-file <path>`
// re-runs it directly with no conversion step and nothing else installed.
// The authoring rows ride along in one extra key, `_embarch_ui_rows`, which
// `Study`'s own deserializer ignores (no `deny_unknown_fields` anywhere in
// that crate) — that's what lets this tab reload a saved study back into an
// editable table instead of only being able to re-run an opaque blob.

/// Maps a human study name onto a filename, and refuses anything that isn't
/// one. Not cosmetic: this string reaches `Path::join`, so `../` or an
/// absolute path would write outside the studies directory entirely.
fn study_slug(name: &str) -> Result<String, String> {
    let slug: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        return Err(format!("'{name}' has no characters usable in a filename"));
    }
    Ok(slug)
}

fn studies_dir(sd: &StudyDesigner) -> std::path::PathBuf {
    sd.0.config.firmware_repo_path.join("embarch").join("studies")
}

#[derive(Debug, Serialize)]
struct SavedStudySummary {
    slug: String,
    name: String,
    steps: usize,
    /// True when the file still carries `_embarch_ui_rows` — a study saved
    /// by this tab. A hand-written or agent-generated `Study` file dropped
    /// into the same directory is still listed and still runnable, it just
    /// can't be loaded back into the table for editing, and the UI says so
    /// rather than silently offering a broken Load.
    editable: bool,
}

#[derive(Debug, Deserialize)]
pub struct SaveStudyRequest {
    name: String,
    rows: Vec<TableRow>,
    requires: RequirementsInput,
    #[serde(default)]
    taps: Vec<TapInput>,
}

#[derive(Debug, Serialize)]
struct LoadedStudy {
    name: String,
    rows: Vec<TableRow>,
    /// Read back out of the saved `Study` itself, not out of a sidecar key —
    /// `requires` is a real field of the thing that runs, so the file is its
    /// own source of truth for it.
    requires: RequirementsOut,
    /// From the sidecar `_embarch_ui_taps`, falling back to reconstructing
    /// what can be reconstructed from `Study.streams`: a study saved before
    /// this key existed, or written by hand, still loads its taps rather than
    /// silently dropping them on the next save.
    taps: Vec<LoadedTap>,
}

#[derive(Debug, Serialize)]
struct RequirementsOut {
    dev_bench_version: String,
    firmware_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoadedTap {
    name: String,
    signal: String,
}

pub async fn api_studies_list(State(state): State<crate::AppState>) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let dir = studies_dir(&sd);

    let mut out: Vec<SavedStudySummary> = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                let slug = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                out.push(SavedStudySummary {
                    name: value
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or(&slug)
                        .to_string(),
                    steps: value.get("steps").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
                    editable: value.get("_embarch_ui_rows").is_some(),
                    slug,
                });
            }
        }
        // A missing directory is an empty library, not an error — nothing
        // creates it until the first save, same posture `ActionRegistry`
        // takes for a missing `study-actions.toml`.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }

    out.sort_by_key(|s| s.name.to_lowercase());
    Json(out).into_response()
}

pub async fn api_studies_save(
    State(state): State<crate::AppState>,
    Json(req): Json<SaveStudyRequest>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let slug = match study_slug(&req.name) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    // Built (and CRC-sealed) before writing, so a saved file is always a
    // valid, immediately-runnable `Study` — a save can't quietly persist
    // rows that would only fail at run time. Same path `run` takes, so the two
    // cannot disagree.
    //
    // **No reflash and no `allow_version_mismatch` reaches this file.** Both
    // are run parameters, not study fields (decision 11: "reflash lives in the
    // run dialog, never in the saved study"), so a saved study cannot carry a
    // waiver into every later re-read of its own results.
    let study = match build_authored(&req.name, &req.rows, &req.requires, &req.taps, &registry) {
        Ok(s) => s,
        Err((code, e)) => return (code, e).into_response(),
    };

    let mut value = match serde_json::to_value(&study) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Study didn't serialize as an object").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match serde_json::to_value(&req.rows) {
        Ok(rows) => {
            value.insert("_embarch_ui_rows".to_string(), rows);
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    // The authored tap rows ride alongside for the same reason the step rows
    // do: `Study.streams` is what runs, and this is what loads back into the
    // table. `Study`'s own deserializer ignores both keys.
    match serde_json::to_value(&req.taps) {
        Ok(taps) => {
            value.insert("_embarch_ui_taps".to_string(), taps);
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }

    let dir = studies_dir(&sd);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("couldn't create {}: {e}", dir.display()))
            .into_response();
    }
    let path = dir.join(format!("{slug}.json"));
    let text = match serde_json::to_string_pretty(&serde_json::Value::Object(value)) {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Err(e) = std::fs::write(&path, text) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("couldn't write {}: {e}", path.display()))
            .into_response();
    }

    Json(serde_json::json!({
        "slug": slug,
        "name": req.name,
        "path": path.to_string_lossy(),
        "steps": req.rows.len(),
    }))
    .into_response()
}

pub async fn api_studies_load(
    State(state): State<crate::AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let slug = match study_slug(&slug) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let path = studies_dir(&sd).join(format!("{slug}.json"));

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, format!("no saved study '{slug}'")).into_response()
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{} isn't valid JSON: {e}", path.display())).into_response(),
    };
    let Some(rows_value) = value.get("_embarch_ui_rows") else {
        return (
            StatusCode::CONFLICT,
            format!(
                "'{slug}' is a runnable Study but wasn't saved from this table, so it has no rows \
                 to load back — run it with `embarch-api run-study --study-file {}`",
                path.display()
            ),
        )
            .into_response();
    };
    let rows: Vec<TableRow> = match serde_json::from_value(rows_value.clone()) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("saved rows couldn't be read back: {e}")).into_response(),
    };

    let requires = RequirementsOut {
        dev_bench_version: version_field(&value, "dev_bench_version"),
        firmware_version: version_field(&value, "firmware_version"),
    };

    let taps = match value.get("_embarch_ui_taps") {
        Some(saved) => serde_json::from_value::<Vec<LoadedTap>>(saved.clone()).unwrap_or_default(),
        None => taps_from_streams(&value),
    };

    Json(LoadedStudy {
        name: value.get("name").and_then(|n| n.as_str()).unwrap_or(&slug).to_string(),
        rows,
        requires,
        taps,
    })
    .into_response()
}

/// Reads one `requires` field back out of a saved `Study`.
///
/// A saved study always has both — `Requirements::validate` refuses a blank
/// one on the way in, so the file cannot hold one. A file that somehow lacks
/// the field loads as `"any"`, which is the reading that matches what such a
/// study would actually have done: nothing constrained it.
fn version_field(value: &serde_json::Value, field: &str) -> String {
    value
        .get("requires")
        .and_then(|r| r.get(field))
        .and_then(|v| v.as_str())
        .unwrap_or(REQUIREMENT_ANY)
        .to_string()
}

/// Recovers authored tap rows from a saved `Study.streams`, for a file with no
/// `_embarch_ui_taps` sidecar — one saved before that key existed, or written
/// by hand.
///
/// Only `StreamSource::Signal` taps come back, because those are the only ones
/// this tab authors. A study carrying a `PowerFrontEnd` or `GattTranscript`
/// tap loads with its steps and *not* its taps, which is the same honest
/// limitation `editable` already reports for a hand-written study's rows —
/// better than presenting a row this table cannot faithfully round-trip.
fn taps_from_streams(value: &serde_json::Value) -> Vec<LoadedTap> {
    let Some(streams) = value.get("streams").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    streams
        .iter()
        .filter_map(|tap| {
            let name = tap.get("name")?.as_str()?.to_string();
            let signal = tap
                .get("source")?
                .get("Signal")?
                .get("name")?
                .as_str()?
                .to_string();
            Some(LoadedTap { name, signal })
        })
        .collect()
}

/// What this bench currently has in front of the operator, for decision 11's
/// prefill — **read live, on request, never cached into an authored study**.
///
/// Prefilling is what makes a mandatory field a help rather than a tax: the
/// common case is "the builds currently in front of me", and typing a hash by
/// hand to express that would guarantee people paste `any` to get past it,
/// defeating the decision.
///
/// Each half fails independently and reports why, because each is unavailable
/// for its own ordinary reason and neither should hide the other: the bench's
/// version needs the bench plugged in and answering a `Hello`, and the DUT's
/// needs a configured firmware repo `git describe` can run in.
///
/// `dev_bench` is the only version string in this suite genuinely read back
/// off the thing it describes. `dut` is not — it is what the working tree
/// says, which is why a study that runs against it gets
/// `VersionSource::Declared` and this tab renders that visibly weaker.
#[derive(Debug, Serialize)]
struct BenchStateResponse {
    dev_bench: Option<String>,
    dev_bench_error: Option<String>,
    dut: Option<String>,
    dut_error: Option<String>,
    /// The literal `"any"`, handed to the browser rather than written there,
    /// so the one string that means "deliberately unconstrained" has exactly
    /// one definition in the suite.
    any: &'static str,
}

pub async fn api_bench_state(State(state): State<crate::AppState>) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };

    let hello = sd.0.core.dev_bench_hello().await;
    let dut = embarch_core_client::version::derive_version(
        &sd.0.config.firmware_repo_path,
        &embarch_core_client::version::default_version_command(),
    )
    .await;

    Json(BenchStateResponse {
        dev_bench: hello.as_ref().ok().map(|h| h.firmware_version.clone()),
        dev_bench_error: hello.as_ref().err().map(|e| format!("{e:#}")),
        dut: dut.as_ref().ok().cloned(),
        dut_error: dut.as_ref().err().map(|e| format!("{e:#}")),
        any: REQUIREMENT_ANY,
    })
    .into_response()
}

/// Whether a `requires` field is satisfied by what the bench currently
/// reports, computed through `embarch-study-designer`'s own
/// `requirement_satisfied` so this tab holds no copy of the comparison rule
/// Core's gate uses.
///
/// Reported rather than enforced: Core's gate is the enforcement point, and
/// showing the discrepancy here — with both strings, before the run — is what
/// decision 11 asks for. A UI that refused the run itself would be a second
/// implementation of a rule Core already owns.
#[derive(Debug, Deserialize)]
pub struct MismatchQuery {
    dev_bench_version: String,
    firmware_version: String,
}

#[derive(Debug, Serialize)]
struct MismatchResponse {
    dev_bench: MismatchField,
    dut: MismatchField,
}

#[derive(Debug, Serialize)]
struct MismatchField {
    required: String,
    actual: Option<String>,
    /// `None` when the actual version could not be read at all — which is not
    /// the same as a mismatch, and must not be rendered as one.
    satisfied: Option<bool>,
    unavailable: Option<String>,
}

pub async fn api_version_check(
    State(state): State<crate::AppState>,
    axum::extract::Query(q): axum::extract::Query<MismatchQuery>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };

    let hello = sd.0.core.dev_bench_hello().await;
    let dut = embarch_core_client::version::derive_version(
        &sd.0.config.firmware_repo_path,
        &embarch_core_client::version::default_version_command(),
    )
    .await;

    let field = |required: &str, actual: Result<String, String>| MismatchField {
        required: required.to_string(),
        satisfied: actual.as_ref().ok().map(|a| requirement_satisfied(required, a)),
        actual: actual.as_ref().ok().cloned(),
        unavailable: actual.err(),
    };

    Json(MismatchResponse {
        dev_bench: field(
            q.dev_bench_version.trim(),
            hello.map(|h| h.firmware_version).map_err(|e| format!("{e:#}")),
        ),
        dut: field(q.firmware_version.trim(), dut.map_err(|e| format!("{e:#}"))),
    })
    .into_response()
}

/// How a run's two versions were established, flattened for the browser.
///
/// `verified` comes from `VersionSource::is_verified()` rather than from a
/// string comparison here: `Declared` must look weaker than
/// `ReportedByDevBench`/`ReportedByOutpost`/`FlashedThisRun`, and which
/// variants count as verified is that enum's own business — a UI re-deriving
/// it is the easiest place to accidentally reintroduce the exact defect
/// decision 40 exists to close (`embarch-ui/design.md` §3 decision 11).
#[derive(Debug, Clone, Serialize)]
pub struct ProvenanceView {
    dev_bench_version: String,
    dev_bench_source: &'static str,
    dev_bench_verified: bool,
    firmware_version: String,
    firmware_source: &'static str,
    firmware_verified: bool,
    /// Every requirement this run was allowed to proceed in spite of, with
    /// both strings — the whole content of an override is the gap between
    /// them.
    overrides: Vec<OverrideView>,
}

#[derive(Debug, Clone, Serialize)]
struct OverrideView {
    subject: &'static str,
    required: String,
    actual: String,
}

fn source_label(source: VersionSource) -> &'static str {
    match source {
        VersionSource::ReportedByDevBench => "reported by dev-bench",
        VersionSource::ReportedByOutpost => "reported by the outpost stream",
        VersionSource::FlashedThisRun => "flashed this run",
        VersionSource::Declared => "declared",
    }
}

pub fn provenance_view(p: &Provenance) -> ProvenanceView {
    ProvenanceView {
        dev_bench_version: p.dev_bench_version.as_str().to_string(),
        dev_bench_source: source_label(p.dev_bench_source),
        dev_bench_verified: p.dev_bench_source.is_verified(),
        firmware_version: p.firmware_version.as_str().to_string(),
        firmware_source: source_label(p.firmware_source),
        firmware_verified: p.firmware_source.is_verified(),
        overrides: p
            .overrides
            .iter()
            .map(|o| OverrideView {
                subject: o.subject.field_name(),
                required: o.required.as_str().to_string(),
                actual: o.actual.as_str().to_string(),
            })
            .collect(),
    }
}

pub async fn api_studies_delete(
    State(state): State<crate::AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    let slug = match study_slug(&slug) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let path = studies_dir(&sd).join(format!("{slug}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Json(serde_json::json!({ "deleted": slug })).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, format!("no saved study '{slug}'")).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---- GATT transcript passthrough ------------------------------------------

/// Serves a finished study's `gatt.csv` (`embarch-study-designer/design.md`
/// §3 decision 36) straight through from Core, so the browser downloads it
/// over embarch-ui's own origin and never needs Core's bearer token — the
/// same reason `/api/enroll` exists rather than the browser calling Core.
pub async fn api_gatt_data(
    State(state): State<crate::AppState>,
    axum::extract::Path(study_id): axum::extract::Path<String>,
) -> axum::response::Response {
    let Some(sd) = state.study_designer else { return not_configured() };
    match sd.0.core.get_study_gatt_data(&study_id).await {
        Ok(bytes) => (
            [
                (axum::http::header::CONTENT_TYPE, "text/csv".to_string()),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"gatt-{study_id}.csv\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(dev_bench: &str, firmware: &str) -> RequirementsInput {
        RequirementsInput {
            dev_bench_version: dev_bench.to_string(),
            firmware_version: firmware.to_string(),
        }
    }

    /// The distinction decision 40 rests on: `"any"` is a deliberate answer
    /// and blank is the not-thought-about case. Turning blank into `"any"`
    /// here would erase it, so blank is refused with the message the UI shows.
    #[test]
    fn a_blank_requirement_is_refused_rather_than_read_as_any() {
        assert_eq!(
            input(REQUIREMENT_ANY, REQUIREMENT_ANY).build().unwrap(),
            Requirements::any()
        );
        for bad in [input("", "v1"), input("v1", ""), input("v1", "   ")] {
            let err = bad.build().expect_err("blank must be refused");
            assert!(err.contains("blank"), "{err}");
            assert!(err.contains("any build"), "the message must name the checkbox: {err}");
        }
    }

    #[test]
    fn a_stated_version_survives_intact() {
        let requires = input("v9-dirty", "g1a2b3c4-dirty").build().unwrap();
        assert_eq!(requires.dev_bench_version.as_str(), "v9-dirty");
        assert_eq!(requires.firmware_version.as_str(), "g1a2b3c4-dirty");
    }

    #[test]
    fn a_version_too_long_for_the_wire_says_so_here() {
        let long = "x".repeat(MAX_FIRMWARE_VERSION_LEN + 1);
        let err = input(&long, REQUIREMENT_ANY).build().expect_err("must refuse");
        assert!(err.contains(&MAX_FIRMWARE_VERSION_LEN.to_string()), "{err}");
    }

    fn tap(name: &str, signal: &str) -> TapInput {
        TapInput { name: name.to_string(), signal: signal.to_string() }
    }

    /// `id` is the wire handle every `StreamOpen`/`StreamChunkBatch`/
    /// `StreamClose` carries, and it must equal the tap's own index. Assigning
    /// it here rather than accepting it is what makes that unfailable.
    #[test]
    fn authored_taps_get_their_index_as_their_wire_handle() {
        let taps = build_taps(&[tap("outpost", "outpost-uart"), tap("second", "other")], 3).unwrap();
        assert_eq!(taps.len(), 2);
        assert_eq!(taps[0].id, 0);
        assert_eq!(taps[1].id, 1);
        assert_eq!(taps[0].name.as_str(), "outpost");
        assert!(matches!(taps[0].encoding, StreamEncoding::OutpostTrace));
        assert!(matches!(taps[0].scope, StreamScope::WholeStudy));
        match &taps[0].source {
            StreamSource::Signal { name } => assert_eq!(name.as_str(), "outpost-uart"),
            other => panic!("a trace tap must name a signal, got {other:?}"),
        }
    }

    /// The pre-flight Core runs on submit, run here so an authoring mistake is
    /// a message in this tab rather than a `400` from a round trip.
    #[test]
    fn the_taps_core_would_reject_are_rejected_here() {
        // Two taps naming the same output file would interleave into one.
        assert!(build_taps(&[tap("outpost", "a"), tap("outpost", "b")], 1).is_err());
        // An unnamed tap has no output file to write to.
        assert!(build_taps(&[tap("  ", "a")], 1).is_err());
        // And a tap that names no signal has no source at all.
        assert!(build_taps(&[tap("outpost", "")], 1).is_err());
    }

    #[test]
    fn no_taps_is_a_valid_study() {
        assert!(build_taps(&[], 2).unwrap().is_empty());
    }

    /// `Declared` has to render visibly weaker than a verified reading, and
    /// which variants count as verified is `VersionSource`'s own business —
    /// re-deriving it in JavaScript is the easiest place to reintroduce the
    /// defect decision 40 closes.
    #[test]
    fn declared_is_the_only_unverified_source() {
        use embarch_study_designer::{VersionOverride, VersionSubject};
        let mut p = Provenance {
            dev_bench_version: HString::try_from("v9").unwrap(),
            firmware_version: HString::try_from("g1a2b3c4").unwrap(),
            dev_bench_source: VersionSource::ReportedByDevBench,
            firmware_source: VersionSource::Declared,
            overrides: HVec::new(),
        };
        let view = provenance_view(&p);
        assert!(view.dev_bench_verified);
        assert_eq!(view.dev_bench_source, "reported by dev-bench");
        assert!(!view.firmware_verified, "Declared must not read as verified");
        assert_eq!(view.firmware_source, "declared");
        assert!(view.overrides.is_empty());

        // An override carries both strings, because the whole content of an
        // override is the gap between them.
        p.overrides
            .push(VersionOverride {
                subject: VersionSubject::Firmware,
                required: HString::try_from("g9999999").unwrap(),
                actual: HString::try_from("g1a2b3c4").unwrap(),
            })
            .unwrap();
        let view = provenance_view(&p);
        assert_eq!(view.overrides.len(), 1);
        assert_eq!(view.overrides[0].subject, "firmware_version");
        assert_eq!(view.overrides[0].required, "g9999999");
        assert_eq!(view.overrides[0].actual, "g1a2b3c4");

        for source in [VersionSource::ReportedByOutpost, VersionSource::FlashedThisRun] {
            assert!(source.is_verified(), "{source:?} reads as unverified");
        }
    }

    /// A study saved before `_embarch_ui_taps` existed, or written by hand,
    /// still loads its taps back rather than silently dropping them on the
    /// next save.
    #[test]
    fn taps_load_back_from_a_study_with_no_sidecar() {
        let study = serde_json::json!({
            "name": "trace run",
            "streams": [
                { "id": 0, "name": "outpost", "source": { "Signal": { "name": "outpost-uart" } },
                  "encoding": "OutpostTrace", "scope": "WholeStudy" },
                // Not a signal tap: this table does not author one, so it does
                // not come back as an editable row rather than coming back
                // wrong.
                { "id": 1, "name": "power", "source": { "PowerFrontEnd": { "sample_hz": 1000 } },
                  "encoding": "Raw", "scope": "WholeStudy" }
            ]
        });
        let taps = taps_from_streams(&study);
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].name, "outpost");
        assert_eq!(taps[0].signal, "outpost-uart");
    }

    #[test]
    fn a_saved_studys_requirements_come_from_the_study_itself() {
        let study = serde_json::json!({
            "requires": { "dev_bench_version": "v9", "firmware_version": "any" }
        });
        assert_eq!(version_field(&study, "dev_bench_version"), "v9");
        assert_eq!(version_field(&study, "firmware_version"), "any");
        // A file with no `requires` at all reads as unconstrained, which is
        // what such a study would in fact have done.
        assert_eq!(version_field(&serde_json::json!({}), "firmware_version"), REQUIREMENT_ANY);
    }
}
