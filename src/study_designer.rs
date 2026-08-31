//! The Study Designer tab's backend (milestone-1.md §4.6): merged action
//! list, custom-action registry, and build/run/watch — all in-process
//! authoring via `embarch-study-designer` (pure/offline, no hardware
//! touched), submission/execution via `embarch-core-client` over HTTP+Bearer
//! (design.md §3 decision 5). Unlike `study-designer-ui`, which shells out
//! to `embarch-api`'s CLI for `run-study`/`study-status`, this talks to
//! `embarch-core` directly through the same shared client the Dashboard/
//! Topology/Enroll tabs already use.
//!
//! **A project can be opened at runtime** (`embarch-ui/design.md` §3
//! decision 14) — this used to say the tab was "disabled entirely (every
//! route below answers `404`) when `[study_designer]` isn't set in config",
//! because milestone-1.md §4.6 resolved via `AskUserQuestion` that a config
//! field, not a UI picker or cwd search, names the firmware repo.
//!
//! That reasoning stands and is not being reversed: the thing it rejected was
//! *guessing* which repo was meant, and it was right that a wrong guess is
//! worse than a clear "not configured" state. An explicit human pick is not a
//! guess, so "Open project" satisfies that reasoning rather than contradicting
//! it. The config field is kept as the zero-click default for a single-repo
//! bench; what changed is that its absence no longer leaves the tab dead.

use crate::config::StudyDesignerConfig;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use embarch_core_client::{CoreClient, StudyRunOptions};
use embarch_study_designer::limits::{
    MAX_DECODERS_PER_STUDY, MAX_FIRMWARE_VERSION_LEN, MAX_SIGNAL_NAME_LEN, MAX_STREAMS_PER_STUDY,
    MAX_STREAM_NAME_LEN,
};
use embarch_study_designer::{
    build_study, merge_actions, requirement_satisfied, validate_taps, Action, ActionRegistry,
    BuiltInActionKind, ZephyrBleDefExtractor, GattConfigExtractor, GattName, GattNameBook,
    GattServiceInfo, Provenance, RegisteredAction, Requirements, RoleChoice, RowAction, Step,
    StreamEncoding, StreamScope, Outcome, StreamSource, StreamTap, StructLayout, StructRegistry,
    Study, StudyResult, TableRow, Uuid, VersionSource, REQUIREMENT_ANY,
};
use heapless::String as HString;
use heapless::Vec as HVec;

/// `Study.streams`' own type, named once rather than spelled out at each use.
type StreamList = HVec<StreamTap, MAX_STREAMS_PER_STUDY>;
/// `Study.decoders`' own type (`embarch-study-designer/design.md` §3
/// decision 52), named once rather than spelled out at each use.
type DecoderList = embarch_study_designer::bounded::Bounded<StructLayout, MAX_DECODERS_PER_STUDY>;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// How long `POST /api/study-designer/discover` waits for a one-step
/// `BleConnect`->`GattDiscover` study to reach a terminal state before
/// giving up — matches `study-designer-ui`'s own precedent
/// (`embarch-study-designer/milestone-11.md` §3.6).
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

struct Inner {
    /// The project currently open — `None` when neither config named one nor
    /// anybody has opened one yet (`embarch-ui/design.md` §3 decision 15).
    ///
    /// A `Mutex`, not a plain field, and that is the whole shape of decision
    /// 14: `firmware_repo_path` is read by three different things (the saved
    /// studies directory, the action registry, the static GATT extractor), so
    /// switching projects has to move all three together or the tab shows one
    /// repo's studies beside another repo's registry. Holding it in one place
    /// is what makes that a single assignment instead of three that can
    /// disagree.
    project: Mutex<Option<StudyDesignerConfig>>,
    core: Arc<CoreClient>,
    /// The most recent live `GattDiscover` result, if any — `None` until
    /// `POST /api/study-designer/discover` has succeeded at least once.
    /// **Cleared when the project changes**: a GATT table discovered against
    /// one repo's DUT says nothing about another's.
    live_gatt: Mutex<Option<Vec<GattServiceInfo>>>,
    /// Cached per project, computed lazily on first need — a repo's source
    /// tree doesn't change while this process is running, but the repo
    /// itself now can. `None` means "not computed for the current project";
    /// `Some(None)` means "computed, and there is no extraction" (no
    /// extractor configured, or it failed), which is a different fact and
    /// must not be recomputed on every request.
    ///
    /// Holds the extraction's *names* alongside its table
    /// (`embarch-study-designer/design.md` §3 decision 56) — cached together
    /// because they come from one text-scan and are invalidated by the same
    /// event, and a name cache that could outlive its table is the
    /// one-repo's-names-beside-another's-UUIDs failure decision 14 already
    /// names in another costume.
    ///
    /// This was a `OnceLock`, which was exactly right while the project was
    /// fixed for the process's lifetime and is exactly wrong now: a
    /// `OnceLock` that has been set cannot be un-set, so the first project's
    /// extraction would have been served for every project after it.
    static_gatt: Mutex<Option<Option<StaticGatt>>>,
    run_tx: watch::Sender<RunState>,
}

#[derive(Clone)]
pub struct StudyDesigner(Arc<Inner>);

impl StudyDesigner {
    /// Constructed unconditionally, with or without a configured project —
    /// the tab's routes now answer for "no project open" instead of the
    /// process having no Study Designer at all (`embarch-ui/design.md` §3
    /// decision 14).
    pub fn new(config: Option<StudyDesignerConfig>, core: Arc<CoreClient>) -> StudyDesigner {
        let (run_tx, _) = watch::channel(RunState::Idle);
        StudyDesigner(Arc::new(Inner {
            project: Mutex::new(config),
            core,
            live_gatt: Mutex::new(None),
            static_gatt: Mutex::new(None),
            run_tx,
        }))
    }

    /// The open project, cloned rather than borrowed: every caller wants a
    /// path to read a file with, and holding the lock across a filesystem
    /// call would serialise requests behind each other for no reason.
    fn project(&self) -> Option<StudyDesignerConfig> {
        self.0.project.lock().unwrap().clone()
    }

    fn repo_path(&self) -> Option<std::path::PathBuf> {
        self.project().map(|p| p.firmware_repo_path)
    }

    /// Switches every project-derived piece of state at once — the point of
    /// decision 14's single `Mutex`. Nothing is cached across the switch:
    /// both the live and the static GATT tables described the *previous*
    /// repo's DUT, and serving either against a new project is the
    /// one-repo's-studies-beside-another's-registry failure in a different
    /// costume.
    fn open_project(&self, config: StudyDesignerConfig) {
        *self.0.project.lock().unwrap() = Some(config);
        *self.0.live_gatt.lock().unwrap() = None;
        *self.0.static_gatt.lock().unwrap() = None;
    }

    fn registry(&self) -> Result<ActionRegistry, String> {
        let Some(repo) = self.repo_path() else {
            return Err(NO_PROJECT.to_string());
        };
        ActionRegistry::load(&repo).map_err(|e| e.to_string())
    }

    /// The firmware repo's own `embarch/study-structs.toml` — the payload
    /// layouts a `GattNotify` tap can decode with
    /// (`embarch-study-designer/design.md` §3 decision 52).
    ///
    /// Read fresh on every call rather than cached, same as
    /// [`Self::registry`]: the file is hand-edited beside the running UI,
    /// and a cached copy would mean a fix to a layout needs a restart to
    /// take effect.
    fn structs(&self) -> Result<StructRegistry, String> {
        let Some(repo) = self.repo_path() else {
            return Err(NO_PROJECT.to_string());
        };
        StructRegistry::load(&repo).map_err(|e| e.to_string())
    }

    /// Runs the configured `static_extractor` at most once per project.
    /// An unrecognized name is a named error the first time it's needed,
    /// not a silent guess — `reference-dut` is the only name this crate
    /// currently ships an extractor for (design.md §3 decision 33).
    fn static_extraction(&self) -> Option<StaticGatt> {
        let mut cached = self.0.static_gatt.lock().unwrap();
        if let Some(computed) = cached.as_ref() {
            return computed.clone();
        }
        let Some(project) = self.project() else {
            // Deliberately not cached: with no project there is nothing to
            // have computed, and caching "no" here would then be served to
            // the project opened a moment later.
            return None;
        };
        let computed = match project.static_extractor.as_deref() {
            Some("zephyr-ble-def") => ZephyrBleDefExtractor
                .extract_labeled(&project.firmware_repo_path)
                .map(|extracted| StaticGatt {
                    services: extracted.services.iter().cloned().collect(),
                    symbols: extracted.characteristic_symbols().collect(),
                    service_symbols: extracted.service_symbols().collect(),
                })
                .map_err(|e| tracing::warn!("static GATT extraction failed: {e}"))
                .ok(),
            Some(other) => {
                tracing::warn!("unrecognized static_extractor '{other}' — only 'reference-dut' exists today");
                None
            }
            None => None,
        };
        *cached = Some(computed.clone());
        computed
    }

    fn static_gatt(&self) -> Option<Vec<GattServiceInfo>> {
        self.static_extraction().map(|extraction| extraction.services)
    }

    /// Every characteristic name this project can resolve
    /// (`embarch-study-designer/design.md` §3 decision 56): the vendor table
    /// unconditionally, plus the firmware's own identifiers when a static
    /// extractor is configured.
    fn names(&self) -> GattNameBook {
        match self.static_extraction() {
            Some(extraction) => GattNameBook::new()
                .with_symbols(extraction.symbols)
                .with_service_symbols(extraction.service_symbols),
            // Not a failure case — a repo with no extractor configured still
            // gets vendor names, which is why this is a book rather than an
            // `Option<Book>`.
            None => GattNameBook::new(),
        }
    }

    fn live_gatt(&self) -> Option<Vec<GattServiceInfo>> {
        self.0.live_gatt.lock().unwrap().clone()
    }
}

/// One static extraction, cached per project: the GATT table plus the C
/// identifiers behind it (`embarch-study-designer/design.md` §3 decision 56).
#[derive(Debug, Clone)]
struct StaticGatt {
    services: Vec<GattServiceInfo>,
    symbols: Vec<(Uuid, String)>,
    /// The identifiers the *services* were declared under
    /// (`embarch-study-designer/design.md` §3 decision 57) — what the
    /// selective-monitor picker's group headers read
    /// (`embarch-ui/design.md` §3 decision 17).
    service_symbols: Vec<(Uuid, String)>,
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

/// Said in one place, because it is both an HTTP body and (via
/// [`StudyDesigner::registry`]) an error string a different layer renders.
const NO_PROJECT: &str = "no project is open — open a firmware repo, or set \
     [study_designer].firmware_repo_path in embarch-ui's config";

/// The answer for a route that needs an open project and hasn't got one.
///
/// Still a `404`, and still the "clear not-configured state" milestone-1.md
/// §4.6 chose over guessing — but it is no longer a dead end, because
/// `POST /api/study-designer/project` is now the way out of it (design.md §3
/// decision 14).
fn not_configured() -> axum::response::Response {
    (StatusCode::NOT_FOUND, NO_PROJECT).into_response()
}

/// The one-click discovery `Study`: connect, then walk the GATT table.
///
/// **`target_name` is an argument rather than the `None` this shipped with,
/// and that `None` was a real defect.** A `BleConnect` with no name connects
/// to *whatever advertises first*, which on a bench with more than one BLE
/// device in the room is a coin flip — and against the DUT this was written
/// for it failed outright, three runs in a row (`connection failed (HCI
/// 0x1f)`, then a disconnect mid-discovery), while the identical study
/// naming the device passed every time. The name is the one fact this
/// action cannot derive and the operator already has: the step table's own
/// `BleConnect` row is holding it.
///
/// `None` is still accepted, because a bench with exactly one device in
/// range is a real case and demanding a name there would be ceremony. It is
/// no longer the *only* option.
fn discover_study(target_name: Option<&str>) -> Result<Study, String> {
    let rows = vec![
        TableRow {
            name: "connect".to_string(),
            action: RowAction::BuiltIn {
                targets: Vec::new(),
                which: BuiltInActionKind::BleConnect,
                role: RoleChoice::Central,
                target_name: target_name.map(|n| n.to_string()),
                security_level: None,
            },
            timeout_ms: 15_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        },
        TableRow {
            name: "discover".to_string(),
            action: RowAction::BuiltIn { targets: Vec::new(), which: BuiltInActionKind::GattDiscover, role: RoleChoice::Central , target_name: None, security_level: None },
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

/// Why a discovery run produced no GATT table, in the words of the step that
/// failed — or `None` when it genuinely succeeded.
///
/// **This exists because the failure used to be invisible.** `api_discover`
/// ended in `first_gatt_services(&result).unwrap_or_default()`, so a study
/// that never reached `GattDiscover` at all stored an *empty* live set,
/// answered `200`, and left the tab reporting live discovery as available
/// with nothing in it. A button that reports success on failure is worse
/// than one that fails, because the operator's next move is to go looking
/// for the bug in their own study.
fn discovery_failure(result: &StudyResult) -> Option<String> {
    for step in result.steps.iter() {
        match &step.outcome {
            Outcome::Pass => continue,
            Outcome::Fail { reason } => {
                return Some(format!("step '{}' failed: {reason}", step.step_name));
            }
            Outcome::TimedOut => {
                return Some(format!("step '{}' timed out", step.step_name));
            }
        }
    }
    // Every step passed and there is still no table: not a failure this can
    // name, but not a success either — say which of the two it is rather
    // than folding it into an empty list.
    first_gatt_services(result)
        .is_none()
        .then(|| "the study completed but reported no GATT table".to_string())
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

/// One tap, as this tab authors it (`embarch-study-designer/design.md` §3
/// decisions 39/52/55, `embarch-outpost/design.md` §3 decisions 11/12).
///
/// Two kinds, distinguished by `kind` rather than by which fields happen to
/// be filled in — an untagged shape would make "no signal" and "no
/// characteristic" the same authoring mistake with two different fixes.
///
/// **An outpost tap names the signal, never the carrier**, which is why it
/// has no port and no pins in it: those live in the signal's declared route
/// (Topology tab), so the identical saved study runs unchanged across a
/// rewiring of the bench. Its `OutpostTrace`/`WholeStudy` encoding and scope
/// are not choices offered — an outpost capture is study-scoped with no live
/// feed by design, and its encoding is the one thing a trace tap can be.
///
/// **A GATT tap names one characteristic and, optionally, the layout to
/// decode its payloads with** (§3 decision 52). Undeclared, its file is raw
/// bytes with no CSV, which is the honest rendering of a payload nobody has
/// described.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TapInput {
    /// An outpost trace on a topology-declared signal.
    Outpost {
        /// The output file's name under the study's `streams/` directory, and
        /// what `GET /study/{id}/stream/{name}` takes.
        name: String,
        /// The declared signal this taps (`SignalLink::name`). Core's
        /// `POST /study` pre-flight rejects a tap naming an undeclared signal
        /// with a `400`, which is why the Topology tab's routes come first.
        signal: String,
    },
    /// One DUT characteristic's notifications, in their own file.
    GattNotify {
        name: String,
        service_uuid: String,
        characteristic_uuid: String,
        /// A `study-structs.toml` entry's name, or blank for raw bytes.
        #[serde(default)]
        decoder: String,
    },
}

impl TapInput {
    /// The output file's name, whichever kind this is.
    fn name(&self) -> &str {
        match self {
            TapInput::Outpost { name, .. } | TapInput::GattNotify { name, .. } => name,
        }
    }
}

/// Turns authored tap rows into `Study.streams`, sealed by the caller.
///
/// `id` is assigned here as the tap's own index, because that is what `id`
/// *is* — the wire handle every `StreamOpen`/`StreamChunkBatch`/`StreamClose`
/// carries — and `validate_taps` rejects any other value. Nothing about that
/// is a choice for an author to make or get wrong.
fn build_taps(
    taps: &[TapInput],
    steps: &[Step],
    structs: &StructRegistry,
) -> Result<(StreamList, DecoderList), String> {
    let mut out: StreamList = StreamList::new();
    let mut decoders: DecoderList = DecoderList::new();

    for (index, tap) in taps.iter().enumerate() {
        let name = tap.name().trim();
        let id =
            u8::try_from(index).map_err(|_| format!("more than {} stream taps", u8::MAX))?;
        let tap_name = HString::try_from(name).map_err(|_| {
            format!("tap name '{name}' is longer than the wire's {MAX_STREAM_NAME_LEN} characters")
        })?;

        let built = match tap {
            TapInput::Outpost { signal, .. } => {
                let signal = signal.trim();
                if signal.is_empty() {
                    return Err(format!("stream tap {} names no signal", index + 1));
                }
                StreamTap {
                    id,
                    name: tap_name,
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
                }
            }
            TapInput::GattNotify { service_uuid, characteristic_uuid, decoder, .. } => {
                let service_uuid = Uuid::parse(service_uuid.trim()).ok_or_else(|| {
                    format!("stream tap {}: '{service_uuid}' is not a UUID", index + 1)
                })?;
                let characteristic_uuid =
                    Uuid::parse(characteristic_uuid.trim()).ok_or_else(|| {
                        format!(
                            "stream tap {}: '{characteristic_uuid}' is not a UUID",
                            index + 1
                        )
                    })?;

                // A tap whose characteristic no step subscribes captures
                // nothing, passes, and looks fine — which is the exact
                // failure decisions 34/36/53/54 were each opened by. Refused
                // here, where the author can fix it, rather than discovered
                // as an empty file after a run.
                if !any_step_subscribes(steps, service_uuid, characteristic_uuid) {
                    return Err(format!(
                        "stream tap '{name}' captures {} , but no step in this study subscribes \
                         to it — add a GattMonitorAll/GattMonitorStart step, or name it in a \
                         selective monitor step's targets",
                        characteristic_uuid.to_hyphenated()
                    ));
                }

                // Resolved here, against the firmware repo's own
                // `study-structs.toml`, so the submitted `Study` carries the
                // layout rather than a name Core has no way to look up
                // (design.md §3 decision 52).
                let encoding = match decoder.trim() {
                    "" => StreamEncoding::Raw,
                    named => {
                        let layout = structs.resolve(named).map_err(|e| e.to_string())?;
                        let existing =
                            decoders.iter().position(|d: &StructLayout| d.name == layout.name);
                        let slot = match existing {
                            Some(at) => at,
                            None => {
                                decoders.push(layout).map_err(|_| {
                                    format!(
                                        "more than {MAX_DECODERS_PER_STUDY} decoders in one study"
                                    )
                                })?;
                                decoders.len() - 1
                            }
                        };
                        StreamEncoding::Struct {
                            decoder: u8::try_from(slot).map_err(|_| "too many decoders")?,
                        }
                    }
                };

                StreamTap {
                    id,
                    name: tap_name,
                    source: StreamSource::GattNotify { service_uuid, characteristic_uuid },
                    encoding,
                    // WholeStudy rather than a step range: a notification
                    // arrives when the DUT sends it, not when a step says so,
                    // and a window authored by hand is one more thing to get
                    // wrong for no gain. The monitor steps already bound when
                    // anything is subscribed at all.
                    scope: StreamScope::WholeStudy,
                }
            }
        };

        out.push(built).map_err(|_| {
            format!("more than {MAX_STREAMS_PER_STUDY} stream taps in one study")
        })?;
    }

    // The same pre-flight Core runs on submit, run here so an authoring
    // mistake is a message in this tab rather than a `400` from a round trip.
    validate_taps(&out, steps.len() as u32, decoders.len()).map_err(|e| format!("{e:?}"))?;
    Ok((out, decoders))
}

/// Whether any step in `steps` subscribes to this characteristic — an
/// unfiltered monitor action (which subscribes to everything notify- or
/// indicate-capable) or a selective one that names it (design.md §3
/// decision 53).
fn any_step_subscribes(steps: &[Step], service: Uuid, characteristic: Uuid) -> bool {
    steps.iter().any(|step| match &step.action {
        Action::GattMonitorAll {} | Action::GattMonitorStart {} => true,
        Action::GattMonitorSelected { targets } | Action::GattMonitorSelectedStart { targets } => {
            targets
                .iter()
                .any(|t| t.service_uuid == service && t.characteristic_uuid == characteristic)
        }
        _ => false,
    })
}

/// The `GattTranscript` tap every study with a monitor step gets, appended
/// after whatever the author declared — `embarch-ui/design.md` §3 decision 15.
///
/// **Auto-declared rather than offered as a checkbox.** Before this, the
/// Study Designer authored no GATT tap at all, so a monitor step's capture
/// existed only as `StepResult.gatt_activity`'s first 32 records — and that
/// field is retired (`embarch-study-designer/design.md` §3 decision 54). A
/// study that monitors and captures nothing is not a configuration anyone
/// wants; making it reachable by leaving a box unticked would only make the
/// old failure re-authorable.
///
/// Skipped when the author already declared a `GattTranscript` tap of their
/// own, and when the study has no monitor step to record.
fn auto_transcript_tap(streams: &mut StreamList, steps: &[Step]) -> Result<(), String> {
    let monitors = steps.iter().any(|step| {
        matches!(
            step.action,
            Action::GattMonitorAll {}
                | Action::GattMonitorStart {}
                | Action::GattMonitorSelected { .. }
                | Action::GattMonitorSelectedStart { .. }
        )
    });
    if !monitors {
        return Ok(());
    }
    if streams.iter().any(|t| matches!(t.source, StreamSource::GattTranscript)) {
        return Ok(());
    }
    let id = u8::try_from(streams.len()).map_err(|_| "more than 255 stream taps".to_string())?;
    streams
        .push(StreamTap {
            id,
            name: HString::try_from(AUTO_TRANSCRIPT_TAP_NAME)
                .expect("AUTO_TRANSCRIPT_TAP_NAME fits MAX_STREAM_NAME_LEN"),
            source: StreamSource::GattTranscript,
            encoding: StreamEncoding::GattTranscript,
            scope: StreamScope::WholeStudy,
        })
        .map_err(|_| {
            format!(
                "this study declares {MAX_STREAMS_PER_STUDY} taps, leaving no room for the GATT \
                 transcript every monitor step needs — remove one"
            )
        })
}

/// The name [`auto_transcript_tap`] gives its tap, and therefore the file a
/// study's full GATT transcript lands in: `streams/gatt.csv`. Chosen to match
/// the retired fixed path that data used to live at, so a reader who knows
/// where to look still looks in the right place.
const AUTO_TRANSCRIPT_TAP_NAME: &str = "gatt";

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
    /// Every notify- or indicate-capable characteristic any discovery source
    /// found — what a selective monitor step picks its targets from, and what
    /// a `GattNotify` tap picks its characteristic from
    /// (`embarch-study-designer/design.md` §3 decisions 53/55).
    ///
    /// A separate list from `actions` rather than a filter over it: `actions`
    /// is keyed by characteristic for *authoring a write*, and its `Vendor`
    /// entries are a compile-time table with no observed properties byte at
    /// all. Whether a characteristic can notify is an observation, and this
    /// list carries only observations.
    subscribable: Vec<SubscribableCharacteristic>,
    /// The names in the firmware repo's `embarch/study-structs.toml` — what a
    /// `GattNotify` tap's decoder dropdown offers (§3 decision 52). Empty when
    /// the repo declares none, which is the ordinary starting state.
    struct_layouts: Vec<String>,
    /// What every picker that names a characteristic labels its options with
    /// (`embarch-study-designer/design.md` §3 decision 56), keyed by
    /// hyphenated characteristic UUID.
    ///
    /// One map for the whole response rather than a `name` field on
    /// `SubscribableCharacteristic`: four different places in the browser
    /// render a characteristic (a selective-monitor checkbox, a GATT tap's
    /// dropdown, a new tap's default file name, an unregistered-characteristic
    /// chip), and three of them read from `actions` rather than from
    /// `subscribable`. A field on one list would have named the options in one
    /// picker and left the same characteristic as a bare UUID in the next.
    ///
    /// Covers every characteristic any source found, not only the
    /// notify-capable ones: a name is a name regardless of what a study can
    /// do with the characteristic.
    characteristic_names: BTreeMap<String, GattName>,
    /// The same thing one level up, keyed by hyphenated **service** UUID
    /// (`embarch-study-designer/design.md` §3 decision 57). What the
    /// selective-monitor picker's group headers read
    /// (`embarch-ui/design.md` §3 decision 17) — a picker that groups by
    /// service needs a name for the group, and `sds_service` is a heading an
    /// engineer can navigate by where `00000001` is not.
    service_names: BTreeMap<String, GattName>,
    /// `limits::MAX_MONITOR_TARGETS` — how many characteristics one selective
    /// monitor step may name (`embarch-study-designer/design.md` §3 decision
    /// 53). Served rather than restated in `app.js`: the cap is enforced by
    /// `build_study`, and a browser-side copy of it is a number that drifts
    /// silently the day the limit moves. The picker
    /// (`embarch-ui/design.md` §3 decision 17) shows it and stops at it, so
    /// the refusal happens where the choice is made rather than as a `400`
    /// after a round trip.
    max_monitor_targets: usize,
}

/// One characteristic a study can subscribe to, as the pickers render it.
#[derive(Debug, Clone, Serialize)]
pub struct SubscribableCharacteristic {
    service_uuid: String,
    characteristic_uuid: String,
    /// The raw ATT properties byte, passed through unchanged — this crate's
    /// standing "raw, not symbolic" stance. The UI renders the bit names.
    properties: u8,
    /// Whether a live discovery saw it, as opposed to only the static source
    /// read out of the firmware repo. A study can name either; a
    /// static-only characteristic behind a disabled Kconfig is exactly the
    /// gap `gatt_extract`'s own doc comment records.
    live: bool,
}

/// Resolves a display name for every characteristic either discovery source
/// found (`embarch-study-designer/design.md` §3 decision 56). A characteristic
/// neither the vendor table nor the firmware's source names is simply absent —
/// the browser renders the UUID for it, exactly as it did for everything
/// before decision 56.
fn characteristic_names(
    names: &GattNameBook,
    live: Option<&[GattServiceInfo]>,
    static_gatt: Option<&[GattServiceInfo]>,
) -> BTreeMap<String, GattName> {
    [live, static_gatt]
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|service| service.characteristics.iter())
        .filter_map(|chrc| {
            names.get(chrc.uuid).map(|name| (chrc.uuid.to_hyphenated().to_string(), name))
        })
        .collect()
}

/// The same, for services (`embarch-study-designer/design.md` §3 decision
/// 57). Separate from `characteristic_names` because the lookup is: a
/// service UUID resolves against the vendor table's *services*, and a
/// merged map would have had to guess which half a UUID wanted.
fn service_names(
    names: &GattNameBook,
    live: Option<&[GattServiceInfo]>,
    static_gatt: Option<&[GattServiceInfo]>,
) -> BTreeMap<String, GattName> {
    [live, static_gatt]
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|service| {
            names.service(service.uuid).map(|name| (service.uuid.to_hyphenated().to_string(), name))
        })
        .collect()
}

/// Flattens discovery results into the subscribable list, live entries first
/// and de-duplicated by characteristic.
fn subscribable_from(
    live: Option<&[GattServiceInfo]>,
    static_gatt: Option<&[GattServiceInfo]>,
) -> Vec<SubscribableCharacteristic> {
    const NOTIFY_OR_INDICATE: u8 = 0x10 | 0x20;
    let mut out: Vec<SubscribableCharacteristic> = Vec::new();
    for (services, is_live) in [(live, true), (static_gatt, false)] {
        for service in services.unwrap_or(&[]) {
            for chrc in &service.characteristics {
                if chrc.properties & NOTIFY_OR_INDICATE == 0 {
                    continue;
                }
                let characteristic_uuid = chrc.uuid.to_hyphenated().to_string();
                if out.iter().any(|c| c.characteristic_uuid == characteristic_uuid) {
                    continue;
                }
                out.push(SubscribableCharacteristic {
                    service_uuid: service.uuid.to_hyphenated().to_string(),
                    characteristic_uuid,
                    properties: chrc.properties,
                    live: is_live,
                });
            }
        }
    }
    out
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
    // A malformed `study-structs.toml` is reported as an empty list plus a
    // logged reason rather than failing this whole route: the action list and
    // the tap pickers are still usable without it, and a tab that renders
    // nothing at all is a worse answer to "one layout has a typo".
    let struct_layouts = match sd.structs() {
        Ok(r) => r.structs.into_iter().map(|d| d.name).collect(),
        Err(e) => {
            tracing::warn!("study-structs.toml could not be read: {e}");
            Vec::new()
        }
    };
    Json(ActionsResponse {
        subscribable: subscribable_from(live.as_deref(), static_gatt.as_deref()),
        characteristic_names: characteristic_names(
            &sd.names(),
            live.as_deref(),
            static_gatt.as_deref(),
        ),
        service_names: service_names(&sd.names(), live.as_deref(), static_gatt.as_deref()),
        max_monitor_targets: embarch_study_designer::limits::MAX_MONITOR_TARGETS,
        actions,
        live_gatt_available: live.is_some(),
        static_gatt_available: static_gatt.is_some(),
        struct_layouts,
    })
    .into_response()
}

pub async fn api_actions(State(state): State<crate::AppState>) -> axum::response::Response {
    let sd = state.study_designer;
    // A project is a precondition rather than an input here — this route
    // reads nothing off it, but it has nothing to answer about without one.
    if sd.project().is_none() {
        return not_configured();
    }
    actions_response(&sd)
}

pub async fn api_registry(State(state): State<crate::AppState>) -> axum::response::Response {
    let sd = state.study_designer;
    // A project is a precondition rather than an input here — this route
    // reads nothing off it, but it has nothing to answer about without one.
    if sd.project().is_none() {
        return not_configured();
    }
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
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let mut registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    registry.actions.retain(|a| a.name != action.name);
    registry.actions.push(action);
    match registry.save(&project.firmware_repo_path) {
        Ok(()) => Json(registry).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// What the Discover button may say about *which* device to talk to.
///
/// Optional, and posted as a body rather than baked in, because the name
/// lives in the step table the operator is already editing — see
/// [`discover_study`] for why a nameless connect was a real defect rather
/// than a simplification.
#[derive(Debug, Default, Deserialize)]
pub struct DiscoverRequest {
    #[serde(default)]
    target_name: Option<String>,
}

pub async fn api_discover(
    State(state): State<crate::AppState>,
    body: Option<Json<DiscoverRequest>>,
) -> axum::response::Response {
    let sd = state.study_designer;
    // A project is a precondition rather than an input here — this route
    // reads nothing off it, but it has nothing to answer about without one.
    if sd.project().is_none() {
        return not_configured();
    }
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let target_name = req.target_name.as_deref().filter(|n| !n.trim().is_empty());
    let mut study = match discover_study(target_name) {
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
            // **A run that found nothing is reported as the failure it was,
            // and the previous live table is left alone.** Overwriting it
            // with `unwrap_or_default()`'s empty list was how a failed
            // discovery came back as `200` with `live_gatt_available: true`
            // and nothing live in it — the tab then showed every
            // characteristic as static-only, which is a different claim
            // about the DUT than "we could not reach it", and the operator
            // has no way to tell the two apart.
            if let Some(why) = discovery_failure(&result) {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("discovery run {study_id} found nothing — {why}"),
                )
                    .into_response();
            }
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
    structs: &StructRegistry,
) -> Result<Study, (StatusCode, String)> {
    let requires = requires.build().map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let mut study = build_study(req_name, requires, rows, registry)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    // Taps are built against the *resolved* steps rather than the raw rows:
    // whether a characteristic is subscribed at all is a property of the
    // `Action` a row became, not of the row's own text.
    let (mut streams, decoders) = build_taps(taps, &study.steps, structs)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    auto_transcript_tap(&mut streams, &study.steps)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    study.streams = streams;
    study.decoders = decoders;
    seal_crc(&mut study).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(study)
}

pub async fn api_run(
    State(state): State<crate::AppState>,
    Json(req): Json<RunRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    // A project is a precondition rather than an input here — this route
    // reads nothing off it, but it has nothing to answer about without one.
    if sd.project().is_none() {
        return not_configured();
    }
    let registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let structs = match sd.structs() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let study =
        match build_authored(&req.name, &req.rows, &req.requires, &req.taps, &registry, &structs) {
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
    let sd = state.study_designer;
    // A project is a precondition rather than an input here — this route
    // reads nothing off it, but it has nothing to answer about without one.
    if sd.project().is_none() {
        return not_configured();
    }
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

/// `<firmware repo>/embarch/studies` (`embarch-study-designer/design.md` §3
/// decision 38) — taken from the *open project* rather than from config, so
/// switching projects moves the studies list with it (`embarch-ui/design.md`
/// §3 decision 14).
fn studies_dir(project: &StudyDesignerConfig) -> std::path::PathBuf {
    project.firmware_repo_path.join("embarch").join("studies")
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

/// A tap as it loads back into the table. **Structurally [`TapInput`]**, and
/// deliberately so: the sidecar this reads is written from `TapInput`, and a
/// second shape that had to agree with it by hand is one more place a saved
/// study and a running one can drift apart.
type LoadedTap = TapInput;

pub async fn api_studies_list(State(state): State<crate::AppState>) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let dir = studies_dir(&project);

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
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
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
    let structs = match sd.structs() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let study =
        match build_authored(&req.name, &req.rows, &req.requires, &req.taps, &registry, &structs) {
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

    let dir = studies_dir(&project);
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
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let slug = match study_slug(&slug) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let path = studies_dir(&project).join(format!("{slug}.json"));

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
/// Only the two sources this tab authors come back — `Signal` and
/// `GattNotify`. A study carrying a `PowerFrontEnd` or `GattTranscript` tap
/// loads with its steps and *not* that tap, which is the same honest
/// limitation `editable` already reports for a hand-written study's rows —
/// better than presenting a row this table cannot faithfully round-trip. The
/// auto-declared `GattTranscript` is re-added on the next build anyway
/// ([`auto_transcript_tap`]), so nothing is lost by leaving it out here.
fn taps_from_streams(value: &serde_json::Value) -> Vec<LoadedTap> {
    let Some(streams) = value.get("streams").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    streams
        .iter()
        .filter_map(|tap| {
            let name = tap.get("name")?.as_str()?.to_string();
            let source = tap.get("source")?;
            if let Some(signal) = source.get("Signal").and_then(|s| s.get("name"))
                .and_then(|n| n.as_str())
            {
                return Some(TapInput::Outpost { name, signal: signal.to_string() });
            }
            let gatt = source.get("GattNotify")?;
            // The decoder name comes back off `Study.decoders[i].name`,
            // which is the `study-structs.toml` entry's own name —
            // `StructRegistry::resolve` carries it onto the layout, so the
            // round trip is a lookup and not a guess. A tap whose encoding is
            // not `Struct`, or whose index is past the decoder list, gets a
            // blank layout: raw bytes, which is what such a tap captures.
            let decoder = value
                .get("decoders")
                .and_then(|d| d.as_array())
                .and_then(|d| {
                    let index = tap.get("encoding")?.get("Struct")?.get("decoder")?.as_u64()?;
                    d.get(index as usize)?.get("name")?.as_str().map(str::to_string)
                })
                .unwrap_or_default();
            Some(TapInput::GattNotify {
                name,
                service_uuid: uuid_field(gatt, "service_uuid")?,
                characteristic_uuid: uuid_field(gatt, "characteristic_uuid")?,
                decoder,
            })
        })
        .collect()
}

/// A `Uuid` in a saved `Study` is a JSON array of 16 bytes (its `Serialize`
/// is the raw form, design.md §4.3); the table works in the hyphenated text
/// an engineer reads. This is the one place that conversion happens on the
/// load path.
fn uuid_field(source: &serde_json::Value, field: &str) -> Option<String> {
    let bytes = source.get(field)?.as_array()?;
    if bytes.len() != 16 {
        return None;
    }
    let mut raw = [0u8; 16];
    for (slot, value) in raw.iter_mut().zip(bytes) {
        *slot = u8::try_from(value.as_u64()?).ok()?;
    }
    Some(Uuid(raw).to_hyphenated().to_string())
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
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };

    let hello = sd.0.core.dev_bench_hello().await;
    let dut = embarch_core_client::version::derive_version(
        &project.firmware_repo_path,
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
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };

    let hello = sd.0.core.dev_bench_hello().await;
    let dut = embarch_core_client::version::derive_version(
        &project.firmware_repo_path,
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
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let slug = match study_slug(&slug) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let path = studies_dir(&project).join(format!("{slug}.json"));
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
    let sd = state.study_designer;
    // A project is a precondition rather than an input here — this route
    // reads nothing off it, but it has nothing to answer about without one.
    if sd.project().is_none() {
        return not_configured();
    }
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

    /// A scratch directory under the system temp dir, unique per test. No
    /// `tempfile` dev-dependency for four lines, matching this crate's
    /// existing posture of spelling small things out.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("embarch-ui-test-{}-{tag}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn uuid(text: &str) -> Uuid {
        Uuid::parse(text).unwrap()
    }

    fn service(service_uuid: &str, characteristic_uuids: &[&str]) -> GattServiceInfo {
        GattServiceInfo {
            uuid: uuid(service_uuid),
            characteristics: characteristic_uuids
                .iter()
                .map(|u| embarch_study_designer::GattCharacteristicInfo {
                    uuid: uuid(u),
                    properties: 0x10,
                })
                .collect(),
        }
    }

    /// `embarch-study-designer/design.md` §3 decision 56: the response names
    /// every characteristic either source found and that anything can name,
    /// from both name sources at once — a vendor characteristic on the live
    /// table and a custom one the firmware source declared.
    #[test]
    fn the_response_names_characteristics_from_both_sources() {
        let live = [service(
            "6e400001-b5a3-f393-e0a9-e50e24dcca9e",
            &["6e400003-b5a3-f393-e0a9-e50e24dcca9e"],
        )];
        let static_gatt = [service(
            "00000001-853f-4a00-8000-e58100000000",
            &["00000002-853f-4a00-8000-e58100000000"],
        )];
        let names = GattNameBook::new().with_symbols([(
            uuid("00000002-853f-4a00-8000-e58100000000"),
            "sds_hrm_rrm_char_uuid".to_string(),
        )]);

        let resolved = characteristic_names(&names, Some(&live), Some(&static_gatt));

        assert_eq!(
            resolved["6e400003-b5a3-f393-e0a9-e50e24dcca9e"].label,
            "NUS TX",
            "a vendor characteristic is named without any extraction at all"
        );
        assert_eq!(resolved["00000002-853f-4a00-8000-e58100000000"].label, "sds_hrm_rrm");
        assert_eq!(resolved.len(), 2);
    }

    /// `embarch-study-designer/design.md` §3 decision 57: the same, one
    /// level up. A picker that groups by service (`embarch-ui/design.md` §3
    /// decision 17) needs a heading, and it comes from the identifier
    /// `parse_gatt_services` already had in hand to resolve the service's
    /// UUID at all.
    #[test]
    fn the_response_names_services_from_both_sources() {
        let live = [service(
            "6e400001-b5a3-f393-e0a9-e50e24dcca9e",
            &["6e400003-b5a3-f393-e0a9-e50e24dcca9e"],
        )];
        let static_gatt = [service(
            "00000020-853f-4a00-8000-e58100000000",
            &["00000021-853f-4a00-8000-e58100000000"],
        )];
        let names = GattNameBook::new().with_service_symbols([(
            uuid("00000020-853f-4a00-8000-e58100000000"),
            "bds_service_uuid".to_string(),
        )]);

        let resolved = service_names(&names, Some(&live), Some(&static_gatt));

        assert_eq!(resolved["6e400001-b5a3-f393-e0a9-e50e24dcca9e"].label, "Nordic UART Service (NUS)");
        assert_eq!(resolved["00000020-853f-4a00-8000-e58100000000"].label, "bds_service");
        assert_eq!(resolved.len(), 2);
    }

    /// Service names and characteristic names are two maps because they are
    /// two lookups: a *characteristic* symbol must never surface as a
    /// service heading, or a grouped picker invents a group.
    #[test]
    fn a_service_nothing_names_is_left_out_rather_than_guessed_at() {
        let static_gatt = [service(
            "00000020-853f-4a00-8000-e58100000000",
            &["00000021-853f-4a00-8000-e58100000000"],
        )];
        // Only the *characteristic* is named.
        let names = GattNameBook::new().with_symbols([(
            uuid("00000021-853f-4a00-8000-e58100000000"),
            "bds_data_char_uuid".to_string(),
        )]);
        assert!(service_names(&names, None, Some(&static_gatt)).is_empty());
        assert_eq!(characteristic_names(&names, None, Some(&static_gatt)).len(), 1);
    }

    /// A characteristic nothing names is **absent**, not present with an
    /// invented label — the browser falls back to the UUID for it, which is
    /// what every picker showed before decision 56.
    #[test]
    fn an_unnamed_characteristic_is_left_out_rather_than_guessed_at() {
        let live = [service(
            "00000001-853f-4a00-8000-e58100000000",
            &["0000dead-853f-4a00-8000-e58100000000"],
        )];
        assert!(characteristic_names(&GattNameBook::new(), Some(&live), None).is_empty());
    }

    /// The two sources overlapping is the ordinary case — the same
    /// characteristic found live *and* in source must not produce two
    /// entries, and the name is the same either way because it is keyed by
    /// UUID rather than by which walk found it.
    #[test]
    fn a_characteristic_found_twice_is_named_once() {
        let both = [service(
            "00000001-853f-4a00-8000-e58100000000",
            &["00000002-853f-4a00-8000-e58100000000"],
        )];
        let names = GattNameBook::new().with_symbols([(
            uuid("00000002-853f-4a00-8000-e58100000000"),
            "sds_hrm_rrm_char_uuid".to_string(),
        )]);
        let resolved = characteristic_names(&names, Some(&both), Some(&both));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved["00000002-853f-4a00-8000-e58100000000"].label, "sds_hrm_rrm");
    }

    /// The distinction the whole "Open project" validation rests on
    /// (design.md §3 decision 14): a firmware repo with no `embarch/`
    /// directory yet is a legitimate first-time state, and must read as
    /// *this repo has no studies yet* rather than *this is not a repo*.
    #[test]
    fn a_repo_with_no_embarch_dir_is_a_repo_with_no_studies() {
        let scratch = Scratch::new("fresh-repo");
        std::fs::create_dir_all(scratch.0.join(".git")).unwrap();
        std::fs::write(scratch.0.join("west.yml"), "manifest:\n").unwrap();

        let survey = survey_project(&scratch.0);
        assert!(survey.is_git_repo);
        assert!(survey.looks_like_firmware);
        assert!(!survey.has_embarch_dir);
        assert!(!survey.has_embarch_config);
        assert!(!survey.has_action_registry);
        assert_eq!(survey.saved_studies, 0);
    }

    /// **Found live, not reasoned about.** The first version of the
    /// acceptance rule treated a bare `embarch/` subdirectory as evidence,
    /// and on this bench that accepted `$HOME` — because the suite's own
    /// parent folder is `~/embarch`, which contains every sub-project and no
    /// firmware. A directory called `embarch` says nothing about its
    /// contents; something `embarch init` or this tab wrote inside it does.
    #[test]
    fn a_bare_embarch_subdirectory_is_not_evidence_of_a_project() {
        let scratch = Scratch::new("suite-parent");
        // What `~/embarch` looks like: an `embarch` directory holding
        // sibling checkouts, none of which is this repo's config.
        std::fs::create_dir_all(scratch.0.join("embarch").join("embarch-core")).unwrap();

        let survey = survey_project(&scratch.0);
        assert!(survey.has_embarch_dir, "the directory is there");
        assert!(!survey.has_embarch_config, "but nothing embarch wrote is in it");
        assert!(!survey.is_git_repo);
        assert!(!survey.looks_like_firmware);
    }

    /// The other side of the same rule: a repo whose only signal is a real
    /// `embarch/` config must still open — that is a project this tab (or
    /// `embarch init`) created, checked out somewhere without a `.git`.
    #[test]
    fn an_embarch_config_alone_is_enough() {
        let scratch = Scratch::new("config-only");
        std::fs::create_dir_all(scratch.0.join("embarch")).unwrap();
        std::fs::write(scratch.0.join("embarch").join("study-actions.toml"), "").unwrap();

        let survey = survey_project(&scratch.0);
        assert!(survey.has_embarch_config);
        assert!(!survey.is_git_repo);
        assert!(!survey.looks_like_firmware);
    }

    #[test]
    fn a_worktree_whose_dot_git_is_a_file_still_counts_as_a_repo() {
        // Checked as an *entry*, not a directory: a git worktree or submodule
        // has `.git` as a plain file, and treating that as "not a repo" would
        // refuse a perfectly ordinary checkout.
        let scratch = Scratch::new("worktree");
        std::fs::write(scratch.0.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert!(survey_project(&scratch.0).is_git_repo);
    }

    #[test]
    fn a_project_with_studies_counts_only_the_json_ones() {
        let scratch = Scratch::new("with-studies");
        let studies = scratch.0.join("embarch").join("studies");
        std::fs::create_dir_all(&studies).unwrap();
        std::fs::write(scratch.0.join("embarch").join("study-actions.toml"), "").unwrap();
        std::fs::write(studies.join("one.json"), "{}").unwrap();
        std::fs::write(studies.join("two.json"), "{}").unwrap();
        // Not a study: an editor backup, a README, anything else that lands
        // in a directory a human can open.
        std::fs::write(studies.join("one.json.bak"), "{}").unwrap();
        std::fs::write(studies.join("README.md"), "notes").unwrap();

        let survey = survey_project(&scratch.0);
        assert!(survey.has_embarch_dir);
        assert!(survey.has_embarch_config);
        assert!(survey.has_action_registry);
        assert_eq!(survey.saved_studies, 2);
    }

    /// A directory with none of the four signals is almost certainly a
    /// mis-typed path — this is the case the refusal exists for, and the one
    /// it must not confuse with the fresh-repo case above.
    #[test]
    fn a_directory_with_no_signal_at_all_looks_like_nothing() {
        let scratch = Scratch::new("not-a-repo");
        std::fs::write(scratch.0.join("holiday.jpg"), "not source").unwrap();

        let survey = survey_project(&scratch.0);
        assert!(!survey.is_git_repo);
        assert!(!survey.looks_like_firmware);
        assert!(!survey.has_embarch_dir);
        assert!(!survey.has_embarch_config);
    }

    /// Decision 14's actual invariant: `firmware_repo_path` is read by the
    /// studies directory, the action registry *and* the static extractor, so
    /// a switch has to move all of them. This asserts the part that can
    /// silently fail to move — the per-project cache — since a stale cache is
    /// how one repo's studies come to sit beside another repo's GATT table.
    #[test]
    fn switching_projects_drops_every_per_project_cache() {
        let core = Arc::new(
            embarch_core_client::CoreClient::new(
                &toml::from_str("base_url = \"http://127.0.0.1:1\"\n").unwrap(),
            )
            .unwrap(),
        );
        let first = Scratch::new("first");
        let second = Scratch::new("second");
        let sd = StudyDesigner::new(
            Some(StudyDesignerConfig {
                firmware_repo_path: first.0.clone(),
                static_extractor: None,
            }),
            core,
        );

        // Stand in for a completed `discover` and a completed static
        // extraction against the first project.
        *sd.0.live_gatt.lock().unwrap() = Some(Vec::new());
        *sd.0.static_gatt.lock().unwrap() =
            Some(Some(StaticGatt {
                services: Vec::new(),
                symbols: Vec::new(),
                service_symbols: Vec::new(),
            }));
        assert!(sd.live_gatt().is_some());

        sd.open_project(StudyDesignerConfig {
            firmware_repo_path: second.0.clone(),
            static_extractor: None,
        });

        assert_eq!(sd.repo_path().as_deref(), Some(second.0.as_path()));
        assert!(sd.live_gatt().is_none(), "a GATT table from the old project must not survive");
        assert!(
            sd.0.static_gatt.lock().unwrap().is_none(),
            "the static-extraction cache must be recomputed for the new project"
        );
        assert_eq!(studies_dir(&sd.project().unwrap()), second.0.join("embarch").join("studies"));
    }

    /// `None` means "not computed yet" and `Some(None)` means "computed, and
    /// there is nothing" — two different facts. Conflating them is what a
    /// `OnceLock` could not express once the project became switchable.
    #[test]
    fn no_configured_extractor_caches_the_absence_rather_than_recomputing() {
        let core = Arc::new(
            embarch_core_client::CoreClient::new(
                &toml::from_str("base_url = \"http://127.0.0.1:1\"\n").unwrap(),
            )
            .unwrap(),
        );
        let scratch = Scratch::new("no-extractor");
        let sd = StudyDesigner::new(
            Some(StudyDesignerConfig {
                firmware_repo_path: scratch.0.clone(),
                static_extractor: None,
            }),
            core,
        );
        assert!(sd.0.static_gatt.lock().unwrap().is_none());
        assert!(sd.static_gatt().is_none());
        let cached = sd.0.static_gatt.lock().unwrap();
        assert!(cached.is_some(), "the computation must be recorded as having happened");
        assert!(cached.as_ref().unwrap().is_none(), "...and as having produced nothing");
    }

    /// With no project open there is nothing to have computed, so the absence
    /// must *not* be cached — otherwise the "no" gets served to the project
    /// opened a moment later.
    #[test]
    fn with_no_project_the_static_extraction_absence_is_not_cached() {
        let core = Arc::new(
            embarch_core_client::CoreClient::new(
                &toml::from_str("base_url = \"http://127.0.0.1:1\"\n").unwrap(),
            )
            .unwrap(),
        );
        let sd = StudyDesigner::new(None, core);
        assert!(sd.static_gatt().is_none());
        assert!(sd.0.static_gatt.lock().unwrap().is_none());
        assert!(sd.registry().is_err(), "no project means no registry to load");
    }

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
        TapInput::Outpost { name: name.to_string(), signal: signal.to_string() }
    }

    const NUS_SERVICE: &str = "6e400001-b5a3-f393-e0a9-e50e24dcca9e";
    const NUS_TX: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9e";

    fn gatt_tap(name: &str, characteristic: &str, decoder: &str) -> TapInput {
        TapInput::GattNotify {
            name: name.to_string(),
            service_uuid: NUS_SERVICE.to_string(),
            characteristic_uuid: characteristic.to_string(),
            decoder: decoder.to_string(),
        }
    }

    /// Steps with one unfiltered monitor action, so a GattNotify tap in
    /// these tests has something subscribing to it.
    fn monitoring_steps() -> Vec<Step> {
        vec![Step {
            name: HString::try_from("monitor").unwrap(),
            action: Action::GattMonitorStart {},
            timeout_ms: 1_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        }]
    }

    fn plain_steps(count: usize) -> Vec<Step> {
        (0..count)
            .map(|i| Step {
                name: HString::try_from(format!("s{i}").as_str()).unwrap(),
                action: Action::GattDiscover {},
                timeout_ms: 1_000,
                continue_on_fail: false,
                delay_before_ms: 0,
            })
            .collect()
    }

    fn structs_toml() -> StructRegistry {
        toml::from_str(
            r#"
[[struct]]
name = "ppg_packet"
header = [{ name = "seq", type = "u16le" }]
repeat = [{ name = "green", type = "i32le" }]
"#,
        )
        .unwrap()
    }

    /// `id` is the wire handle every `StreamOpen`/`StreamChunkBatch`/
    /// `StreamClose` carries, and it must equal the tap's own index. Assigning
    /// it here rather than accepting it is what makes that unfailable.
    #[test]
    fn authored_taps_get_their_index_as_their_wire_handle() {
        let (taps, decoders) = build_taps(
            &[tap("outpost", "outpost-uart"), tap("second", "other")],
            &plain_steps(3),
            &StructRegistry::default(),
        )
        .unwrap();
        assert_eq!(taps.len(), 2);
        assert!(decoders.is_empty(), "an outpost tap declares no decoder");
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
        let steps = plain_steps(1);
        let none = StructRegistry::default();
        // Two taps naming the same output file would interleave into one.
        assert!(build_taps(&[tap("outpost", "a"), tap("outpost", "b")], &steps, &none).is_err());
        // An unnamed tap has no output file to write to.
        assert!(build_taps(&[tap("  ", "a")], &steps, &none).is_err());
        // And a tap that names no signal has no source at all.
        assert!(build_taps(&[tap("outpost", "")], &steps, &none).is_err());
    }

    #[test]
    fn no_taps_is_a_valid_study() {
        assert!(build_taps(&[], &plain_steps(2), &StructRegistry::default())
            .unwrap()
            .0
            .is_empty());
    }

    // ---- GattNotify taps (design.md §3 decisions 52/55) ----

    #[test]
    fn a_gatt_tap_with_a_named_layout_resolves_it_into_the_study() {
        // The submitted `Study` carries the layout, not the name: Core cannot
        // read the firmware repo, so a study that named a layout without
        // carrying it would render nothing on any machine but this one.
        let (taps, decoders) = build_taps(
            &[gatt_tap("ppg", NUS_TX, "ppg_packet")],
            &monitoring_steps(),
            &structs_toml(),
        )
        .unwrap();
        assert_eq!(decoders.len(), 1);
        assert_eq!(decoders[0].name.as_str(), "ppg_packet");
        assert!(matches!(taps[0].encoding, StreamEncoding::Struct { decoder: 0 }));
        match taps[0].source {
            StreamSource::GattNotify { characteristic_uuid, .. } => {
                assert_eq!(characteristic_uuid, Uuid::parse(NUS_TX).unwrap());
            }
            ref other => panic!("expected a GattNotify source, got {other:?}"),
        }
    }

    #[test]
    fn a_gatt_tap_with_no_layout_is_raw_rather_than_guessed_at() {
        let (taps, decoders) =
            build_taps(&[gatt_tap("ppg", NUS_TX, "")], &monitoring_steps(), &structs_toml())
                .unwrap();
        assert!(decoders.is_empty());
        assert!(matches!(taps[0].encoding, StreamEncoding::Raw));
    }

    #[test]
    fn two_taps_sharing_a_layout_share_one_decoder_slot() {
        let (taps, decoders) = build_taps(
            &[gatt_tap("a", NUS_TX, "ppg_packet"), gatt_tap("b", NUS_SERVICE, "ppg_packet")],
            &monitoring_steps(),
            &structs_toml(),
        )
        .unwrap();
        assert_eq!(decoders.len(), 1, "one layout, one slot");
        assert!(matches!(taps[0].encoding, StreamEncoding::Struct { decoder: 0 }));
        assert!(matches!(taps[1].encoding, StreamEncoding::Struct { decoder: 0 }));
    }

    #[test]
    fn a_gatt_tap_nothing_subscribes_to_is_refused_at_authoring_time() {
        // A tap whose characteristic no step subscribes captures nothing,
        // passes, and looks fine — the failure decisions 34/36/53/54 were
        // each opened by. Caught where the author can fix it.
        let err = build_taps(&[gatt_tap("ppg", NUS_TX, "")], &plain_steps(1), &structs_toml())
            .expect_err("must refuse");
        assert!(err.contains("no step in this study subscribes"), "{err}");
    }

    #[test]
    fn a_selective_monitor_step_satisfies_only_the_characteristics_it_names() {
        let mut steps = plain_steps(0);
        let mut targets = embarch_study_designer::bounded::Bounded::new();
        targets
            .push(embarch_study_designer::GattTarget {
                service_uuid: Uuid::parse(NUS_SERVICE).unwrap(),
                characteristic_uuid: Uuid::parse(NUS_TX).unwrap(),
            })
            .unwrap();
        steps.push(Step {
            name: HString::try_from("monitor").unwrap(),
            action: Action::GattMonitorSelectedStart { targets },
            timeout_ms: 1_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        });

        assert!(build_taps(&[gatt_tap("tx", NUS_TX, "")], &steps, &structs_toml()).is_ok());
        // The RX characteristic is not named by that step, so a tap on it
        // would capture nothing.
        let rx = "6e400002-b5a3-f393-e0a9-e50e24dcca9e";
        assert!(build_taps(&[gatt_tap("rx", rx, "")], &steps, &structs_toml()).is_err());
    }

    #[test]
    fn a_layout_the_repo_does_not_declare_is_named_not_silently_dropped() {
        let err = build_taps(
            &[gatt_tap("ppg", NUS_TX, "ecg_packet")],
            &monitoring_steps(),
            &structs_toml(),
        )
        .expect_err("must refuse");
        assert!(err.contains("ecg_packet"), "{err}");
    }

    // ---- the auto-declared transcript tap (design.md §3 decision 14) ----

    #[test]
    fn a_study_with_a_monitor_step_gets_a_transcript_tap_it_did_not_author() {
        // Before this, the Study Designer authored no GATT tap at all, so a
        // monitor step's capture existed only as the (now retired)
        // `StepResult.gatt_activity`'s first 32 records.
        let mut streams: StreamList = StreamList::new();
        auto_transcript_tap(&mut streams, &monitoring_steps()).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].name.as_str(), "gatt");
        assert_eq!(streams[0].id, 0, "id is still the tap's own index");
        assert!(matches!(streams[0].source, StreamSource::GattTranscript));
        assert!(matches!(streams[0].encoding, StreamEncoding::GattTranscript));
    }

    #[test]
    fn a_study_with_no_monitor_step_gets_no_transcript_tap() {
        let mut streams: StreamList = StreamList::new();
        auto_transcript_tap(&mut streams, &plain_steps(2)).unwrap();
        assert!(streams.is_empty(), "nothing to record, nothing declared");
    }

    #[test]
    fn the_auto_tap_lands_after_the_authored_ones_and_keeps_index_as_id() {
        let (mut streams, _) = build_taps(
            &[gatt_tap("ppg", NUS_TX, "ppg_packet")],
            &monitoring_steps(),
            &structs_toml(),
        )
        .unwrap();
        auto_transcript_tap(&mut streams, &monitoring_steps()).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[1].id, 1);
        assert_eq!(
            validate_taps(&streams, 1, 1),
            Ok(()),
            "the auto tap must satisfy the same pre-flight Core runs"
        );
    }

    #[test]
    fn an_authored_transcript_tap_is_not_duplicated() {
        let mut streams: StreamList = StreamList::new();
        streams
            .push(StreamTap {
                id: 0,
                name: HString::try_from("my-transcript").unwrap(),
                source: StreamSource::GattTranscript,
                encoding: StreamEncoding::GattTranscript,
                scope: StreamScope::WholeStudy,
            })
            .unwrap();
        auto_transcript_tap(&mut streams, &monitoring_steps()).unwrap();
        assert_eq!(streams.len(), 1, "one producer per transcript, and the author's wins");
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
        match &taps[0] {
            TapInput::Outpost { name, signal } => {
                assert_eq!(name, "outpost");
                assert_eq!(signal, "outpost-uart");
            }
            other => panic!("expected an outpost tap, got {other:?}"),
        }
    }

    /// A GATT tap loads back with both UUIDs in the hyphenated form the
    /// table works in, and — when the study carries the layout — with its
    /// decoder name.
    #[test]
    fn a_gatt_tap_loads_back_from_a_study_with_no_sidecar() {
        let service: Vec<u8> = Uuid::parse(NUS_SERVICE).unwrap().0.to_vec();
        let characteristic: Vec<u8> = Uuid::parse(NUS_TX).unwrap().0.to_vec();
        let study = serde_json::json!({
            "name": "capture",
            "decoders": [{ "name": "ppg_packet", "header": [], "repeat": [] }],
            "streams": [{
                "id": 0,
                "name": "ppg",
                "source": { "GattNotify": {
                    "service_uuid": service,
                    "characteristic_uuid": characteristic,
                } },
                "encoding": { "Struct": { "decoder": 0 } },
                "scope": "WholeStudy"
            }]
        });
        match &taps_from_streams(&study)[0] {
            TapInput::GattNotify { name, service_uuid, characteristic_uuid, decoder } => {
                assert_eq!(name, "ppg");
                assert_eq!(service_uuid, NUS_SERVICE);
                assert_eq!(characteristic_uuid, NUS_TX);
                assert_eq!(decoder, "ppg_packet");
            }
            other => panic!("expected a GattNotify tap, got {other:?}"),
        }
    }

    #[test]
    fn a_raw_gatt_tap_loads_back_with_no_decoder_rather_than_a_guessed_one() {
        let study = serde_json::json!({
            "streams": [{
                "id": 0,
                "name": "ppg",
                "source": { "GattNotify": {
                    "service_uuid": Uuid::parse(NUS_SERVICE).unwrap().0.to_vec(),
                    "characteristic_uuid": Uuid::parse(NUS_TX).unwrap().0.to_vec(),
                } },
                "encoding": "Raw",
                "scope": "WholeStudy"
            }]
        });
        match &taps_from_streams(&study)[0] {
            TapInput::GattNotify { decoder, .. } => assert!(decoder.is_empty()),
            other => panic!("expected a GattNotify tap, got {other:?}"),
        }
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

// ---- projects (design.md §3 decision 14) ------------------------------------

/// One entry of the recent-projects list, and one row of the "Open project"
/// panel. `static_extractor` rides along so reopening a project restores the
/// whole `StudyDesignerConfig`, not just the path — a repo whose GATT table
/// only exists in source is useless without it, and re-typing it every time
/// would be the busywork this list exists to remove.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentProject {
    path: String,
    #[serde(default)]
    static_extractor: Option<String>,
}

/// How many recent projects are kept. Small on purpose: this is a
/// convenience list an engineer scans, not a history to search.
const MAX_RECENT_PROJECTS: usize = 8;

/// `<per-user data dir>/embarch/ui/recent-projects.json`, or whatever
/// `EMBARCH_UI_STATE` names.
///
/// **Not the config file.** `EMBARCH_UI_CONFIG` is a file an engineer writes
/// and this process only reads; writing a recent-projects list into it would
/// mean rewriting a human's own file (comments and all) to record a UI
/// convenience. The per-user data directory is where this suite already keeps
/// process-written state (`embarch-core-client::user_dirs`, shared with
/// `embarch-api`'s logfile), so it goes there.
fn recent_projects_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("EMBARCH_UI_STATE") {
        return Some(std::path::PathBuf::from(explicit));
    }
    embarch_core_client::user_dirs::user_data_dir()
        .map(|dir| dir.join("ui").join("recent-projects.json"))
        .map_err(|e| tracing::warn!("no per-user data dir for the recent-projects list: {e:#}"))
        .ok()
}

/// An unreadable or unparseable file is an empty list, logged, never an
/// error: a convenience list that can refuse to start the tab would be worse
/// than no list at all.
fn load_recent_projects() -> Vec<RecentProject> {
    let Some(path) = recent_projects_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    match serde_json::from_str::<Vec<RecentProject>>(&text) {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("ignoring unreadable recent-projects list at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Most-recent-first, deduplicated by path, capped. Failure is logged and
/// swallowed for the same reason `load_recent_projects` tolerates a bad file:
/// a project that opened fine must not report failure because a convenience
/// list could not be written.
fn remember_recent_project(entry: &RecentProject) {
    let Some(path) = recent_projects_path() else { return };
    let mut list = load_recent_projects();
    list.retain(|e| e.path != entry.path);
    list.insert(0, entry.clone());
    list.truncate(MAX_RECENT_PROJECTS);

    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&list)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&path, text)
    };
    if let Err(e) = write() {
        tracing::warn!("couldn't write the recent-projects list at {}: {e}", path.display());
    }
}

/// What a directory looks like from the Study Designer's point of view — the
/// answer to "is this a firmware repo, and does it have any studies yet".
///
/// **The two questions are separate, and conflating them was the trap.** A
/// firmware repo with no `embarch/` directory is a completely legitimate
/// first-time state — `api_studies_list` already tolerates a missing studies
/// directory and `api_studies_save` creates it on first save — so "no
/// `embarch/` here" must read as *this repo has no studies yet*, never as
/// *this is not a repo*.
#[derive(Debug, Serialize)]
pub struct ProjectSurvey {
    /// A `.git` entry is present. Checked as an *entry*, not as a directory:
    /// a git worktree or a submodule has `.git` as a plain file.
    is_git_repo: bool,
    /// Something a firmware build would recognise (`west.yml`,
    /// `CMakeLists.txt`, `prj.conf`, `Cargo.toml`). Not required, and not
    /// exhaustive — it is one of several signals that a directory is a
    /// source repo rather than someone's home directory.
    looks_like_firmware: bool,
    /// `<repo>/embarch` exists. Absence is a first-time state, not a fault.
    ///
    /// **On its own this is not evidence of anything**, which a live run
    /// found the hard way: pointing "Open project" at `$HOME` was accepted,
    /// because this bench's own suite parent folder is `~/embarch` and a
    /// directory called `embarch` is not a claim about its contents. It is
    /// reported, and it is *not* one of the signals that makes a directory
    /// acceptable — `has_embarch_config` below is.
    has_embarch_dir: bool,
    /// `<repo>/embarch` holds something this tab or `embarch init` actually
    /// put there: `study-actions.toml`, `studies/`, or `embarch.toml`. This
    /// *is* a signal, where the bare directory is not.
    has_embarch_config: bool,
    /// `<repo>/embarch/study-actions.toml` exists
    /// (`embarch-study-designer/design.md` §3 decision 34's registry).
    has_action_registry: bool,
    /// How many `*.json` files `<repo>/embarch/studies` holds. `0` with
    /// `has_embarch_dir: false` is the first-time state; `0` with it true is
    /// a project whose studies were all deleted.
    saved_studies: usize,
}

fn survey_project(repo: &std::path::Path) -> ProjectSurvey {
    let embarch = repo.join("embarch");
    let studies = embarch.join("studies");
    ProjectSurvey {
        is_git_repo: repo.join(".git").exists(),
        looks_like_firmware: ["west.yml", "CMakeLists.txt", "prj.conf", "Cargo.toml"]
            .iter()
            .any(|f| repo.join(f).exists()),
        has_embarch_dir: embarch.is_dir(),
        has_embarch_config: ["study-actions.toml", "studies", "embarch.toml"]
            .iter()
            .any(|f| embarch.join(f).exists()),
        has_action_registry: embarch.join("study-actions.toml").is_file(),
        saved_studies: std::fs::read_dir(&studies)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
                    .count()
            })
            .unwrap_or(0),
    }
}

/// What the "Open project" panel renders. `project` is `None` when nothing is
/// open, which is a state the panel exists to get *out of* rather than a
/// failure to report.
#[derive(Debug, Serialize)]
pub struct ProjectState {
    path: Option<String>,
    static_extractor: Option<String>,
    /// `<repo>/embarch/studies`, spelled out rather than left for the browser
    /// to join — one definition of the layout
    /// (`embarch-study-designer/design.md` §3 decision 38), server-side.
    studies_dir: Option<String>,
    survey: Option<ProjectSurvey>,
    recents: Vec<RecentProject>,
}

pub async fn api_project(State(state): State<crate::AppState>) -> axum::response::Response {
    let sd = state.study_designer;
    let project = sd.project();
    let body = ProjectState {
        path: project.as_ref().map(|p| p.firmware_repo_path.to_string_lossy().into_owned()),
        static_extractor: project.as_ref().and_then(|p| p.static_extractor.clone()),
        studies_dir: project
            .as_ref()
            .map(|p| studies_dir(p).to_string_lossy().into_owned()),
        survey: project.as_ref().map(|p| survey_project(&p.firmware_repo_path)),
        recents: load_recent_projects(),
    };
    Json(body).into_response()
}

#[derive(Debug, Deserialize)]
pub struct OpenProjectRequest {
    path: String,
    #[serde(default)]
    static_extractor: Option<String>,
}

/// Opens a firmware repo by path, after checking it is one.
///
/// **A typed path, validated server-side, is the honest shape here** and not
/// a compromise. A browser has no directory picker that yields a usable path
/// — a `<input type="file" webkitdirectory>` hands back file *names* with no
/// directory, and even a real path would still have to be resolved on the
/// server, because the server is what reads the files. So the choice was
/// between a typed path with a real check plus a recents list, and a picker
/// that looks better and cannot work. This is the first.
pub async fn api_open_project(
    State(state): State<crate::AppState>,
    Json(req): Json<OpenProjectRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let raw = req.path.trim();
    if raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "no path given").into_response();
    }
    let repo = std::path::PathBuf::from(raw);
    if !repo.exists() {
        return (StatusCode::BAD_REQUEST, format!("{} doesn't exist", repo.display()))
            .into_response();
    }
    if !repo.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            format!("{} is a file, not a directory", repo.display()),
        )
            .into_response();
    }

    let survey = survey_project(&repo);
    // Refused only when *nothing* says "source repo". A repo with no
    // `embarch/` yet passes on `.git` alone, which is the first-time state
    // this must not reject; a directory with none of these signals is almost
    // certainly a mis-typed path, and naming what was looked for is more
    // useful than "invalid".
    //
    // **`has_embarch_dir` is deliberately not in this list, and a live run
    // is why.** The first version accepted any directory containing an
    // `embarch/` subdirectory — and on this bench that accepted `$HOME`,
    // because the suite's own parent folder is `~/embarch`. A directory
    // called `embarch` is not a statement about its contents;
    // `has_embarch_config` (something this tab or `embarch init` actually
    // wrote there) is.
    if !survey.is_git_repo && !survey.looks_like_firmware && !survey.has_embarch_config {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "{} doesn't look like a firmware repo — no .git, nothing under embarch/ that \
                 embarch put there, and none of west.yml / CMakeLists.txt / prj.conf / \
                 Cargo.toml. A repo with no embarch/ directory yet is fine (it gets created on \
                 the first save); a directory with none of these probably isn't the one you \
                 meant.",
                repo.display()
            ),
        )
            .into_response();
    }

    // Canonicalised so the recents list doesn't accumulate three spellings of
    // the same repo. Falls back to what was typed if canonicalisation fails,
    // which it can on a path behind a broken symlink — refusing an otherwise
    // usable directory over that would be worse.
    let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
    let static_extractor = req
        .static_extractor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    sd.open_project(StudyDesignerConfig {
        firmware_repo_path: repo.clone(),
        static_extractor: static_extractor.clone(),
    });
    let entry = RecentProject {
        path: repo.to_string_lossy().into_owned(),
        static_extractor: static_extractor.clone(),
    };
    remember_recent_project(&entry);

    let survey = survey_project(&repo);
    Json(ProjectState {
        path: Some(entry.path.clone()),
        static_extractor,
        studies_dir: Some(
            repo.join("embarch").join("studies").to_string_lossy().into_owned(),
        ),
        survey: Some(survey),
        recents: load_recent_projects(),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct NewStudyRequest {
    name: String,
}

/// Creates a new, empty, **immediately valid and immediately runnable**
/// saved study in the open project.
///
/// Everything a `Study` needs in order to round-trip is supplied here rather
/// than left for the author to discover on their first save: `requires` is
/// mandatory with no serde default, so it is written as an explicit
/// `REQUIREMENT_ANY` on both fields (`embarch-study-designer/design.md` §3
/// decision 40 — "I don't care which build" is a real answer that has to be
/// *said*); both CRCs are sealed by the same `build_authored` a save uses, so
/// the file cannot be a shape only this route produces. A new study with no
/// steps is legal and does nothing, which is what "new" means.
///
/// Refuses to overwrite: a name whose slug already exists is a `409`, not a
/// silent replacement of somebody's work.
pub async fn api_new_study(
    State(state): State<crate::AppState>,
    Json(req): Json<NewStudyRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let slug = match study_slug(&req.name) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let dir = studies_dir(&project);
    let path = dir.join(format!("{slug}.json"));
    if path.exists() {
        return (
            StatusCode::CONFLICT,
            format!("'{slug}' already exists — open it, or pick another name"),
        )
            .into_response();
    }
    let registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let study = match build_authored(
        &req.name,
        &[],
        &RequirementsInput::any(),
        &[],
        &registry,
        &StructRegistry::default(),
    ) {
        Ok(s) => s,
        Err((code, e)) => return (code, e).into_response(),
    };

    let mut value = match serde_json::to_value(&study) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Study didn't serialize as an object")
                .into_response()
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // The same two sidecar keys `api_studies_save` writes, empty — so the new
    // file is `editable` in `api_studies_list`'s sense from the moment it
    // exists, and Load works on it rather than reporting it as a runnable
    // study this table can't edit.
    value.insert("_embarch_ui_rows".to_string(), serde_json::json!([]));
    value.insert("_embarch_ui_taps".to_string(), serde_json::json!([]));

    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't create {}: {e}", dir.display()),
        )
            .into_response();
    }
    let text = match serde_json::to_string_pretty(&serde_json::Value::Object(value)) {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Err(e) = std::fs::write(&path, text) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write {}: {e}", path.display()),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "slug": slug,
        "name": req.name,
        "path": path.to_string_lossy(),
        "steps": 0,
    }))
    .into_response()
}
