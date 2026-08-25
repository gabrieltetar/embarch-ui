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
use embarch_core_client::CoreClient;
use embarch_study_designer::{
    build_study, merge_actions, ActionRegistry, BuiltInActionKind, ZephyrBleDefExtractor,
    Requirements,
    GattConfigExtractor, GattServiceInfo, RegisteredAction, RoleChoice, RowAction, Study, StudyResult,
    TableRow,
};
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
    Completed { study_id: String, result: Box<StudyResult> },
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
    build_study(
        "embarch-ui discover",
        authoring_requirements(),
        &rows,
        &ActionRegistry::default(),
    )
    .map_err(|e| e.to_string())
}

/// What this tab currently declares a study requires
/// (`embarch-study-designer/design.md` §3 decision 40).
///
/// **Explicitly `any` for both, not omitted** — that decision makes both
/// fields mandatory precisely so "I don't care which build" has to be said
/// rather than reached by leaving a field out. Saying it here is honest
/// about today's state: this tab has no fields for a human to state a real
/// requirement in yet, and prefilling them from live bench state is
/// Milestone 7 Phase D's work (`embarch-ui/design.md` §3 decision 11,
/// `embarch-outpost/milestone-1.md` §5). Until those fields exist, a study
/// authored here genuinely does not constrain the builds it runs against,
/// and its `StudyResult.provenance` will say `Declared` accordingly.
fn authoring_requirements() -> Requirements {
    Requirements::any()
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
    let study_id = match sd.0.core.post_study(&study).await {
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
    let mut study = match build_study(&req.name, authoring_requirements(), &req.rows, &registry) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    if let Err(e) = seal_crc(&mut study) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let study_id = match sd.0.core.post_study(&study).await {
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
                        Some(result) => RunState::Completed { study_id, result: Box::new(result) },
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
}

#[derive(Debug, Serialize)]
struct LoadedStudy {
    name: String,
    rows: Vec<TableRow>,
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
    // rows that would only fail at run time.
    let mut study = match build_study(&req.name, authoring_requirements(), &req.rows, &registry) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    if let Err(e) = seal_crc(&mut study) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

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

    Json(LoadedStudy {
        name: value.get("name").and_then(|n| n.as_str()).unwrap_or(&slug).to_string(),
        rows,
    })
    .into_response()
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
