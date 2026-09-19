//! The Study Designer tab's backend (`embarch-doc/embarch-ui/spec.md`): merged action
//! list, custom-action registry, and build/run/watch — all in-process
//! authoring via `embarch-study-designer` (pure/offline, no hardware
//! touched), submission/execution via `embarch-core-client` over HTTP+Bearer
//! (decision 5). Unlike `study-designer-ui`, which shells out
//! to `embarch-api`'s CLI for `run-study`/`study-status`, this talks to
//! `embarch-core` directly through the same shared client the Dashboard/
//! Topology/Enroll tabs already use.
//!
//! **A project can be opened at runtime** (decision 14) — this used to
//! say the tab was "disabled entirely (every
//! route below answers `404`) when `[study_designer]` isn't set in config",
//! because decision 14's predecessor resolved via `AskUserQuestion` that a config
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
use axum::response::{IntoResponse, Json};
use embarch_core_client::{CoreClient, StudyRunOptions};
use embarch_study_designer::limits::{
    MAX_BUILD_EXTRA_ARGS, MAX_BUILD_EXTRA_ARG_LEN, MAX_BUILD_TARGET_FIELD_LEN,
    MAX_DECODERS_PER_STUDY, MAX_FIRMWARE_VERSION_LEN, MAX_RECORD_MAGIC_LEN, MAX_SIGNAL_NAME_LEN,
    MAX_SNIPPETS_PER_BUILD, MAX_SNIPPET_NAME_LEN, MAX_STREAMS_PER_STUDY, MAX_STREAM_NAME_LEN,
};
use embarch_study_designer::eap_repo::RepoProtocols;
use embarch_study_designer::{DevBenchLogLevel, ProtocolDef, RecordCheck, RecordFraming};
use embarch_study_designer::{
    build_study, merge_actions, requirement_satisfied, validate_taps, Action, ActionRegistry,
    BuildSpec, BuiltInActionKind, ZephyrBleDefExtractor, GattConfigExtractor, GattName,
    GattNameBook, GattServiceInfo, OutpostModeRequirement, Provenance, RegisteredAction,
    Requirements, RoleChoice, RowAction, Step,
    StreamEncoding, StreamScope, Outcome, StreamSource, StreamTap, StructLayout, StructRegistry,
    Study, StudyResult, TableRow, Uuid, VersionSource, REQUIREMENT_ANY,
};
use heapless::String as HString;
use heapless::Vec as HVec;

/// `Study.streams`' own type, named once rather than spelled out at each use.
type StreamList = HVec<StreamTap, MAX_STREAMS_PER_STUDY>;
/// `Study.decoders`' own type (`embarch-study-designer` decision 52),
/// named once rather than spelled out at each use.
type DecoderList = embarch_study_designer::bounded::Bounded<StructLayout, MAX_DECODERS_PER_STUDY>;
/// `Study.record_checks`' own type (`embarch-study-designer` decision 70).
/// Bounded by the tap count, not by a cap of its own: a check rides on a tap.
type RecordCheckList = embarch_study_designer::bounded::Bounded<RecordCheck, MAX_STREAMS_PER_STUDY>;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long `POST /api/study-designer/discover` waits for a one-step
/// `BleConnect`->`GattDiscover` study to reach a terminal state before
/// giving up — matches `study-designer-ui`'s own precedent.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

struct Inner {
    /// The project currently open — `None` when neither config named one nor
    /// anybody has opened one yet (decision 14).
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
    /// (`embarch-study-designer` decision 56) — cached together
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
}

#[derive(Clone)]
pub struct StudyDesigner(Arc<Inner>);

impl StudyDesigner {
    /// Constructed unconditionally, with or without a configured project —
    /// the tab's routes now answer for "no project open" instead of the
    /// process having no Study Designer at all (decision 14).
    pub fn new(config: Option<StudyDesignerConfig>, core: Arc<CoreClient>) -> StudyDesigner {
        StudyDesigner(Arc::new(Inner {
            project: Mutex::new(config),
            core,
            live_gatt: Mutex::new(None),
            static_gatt: Mutex::new(None),
        }))
    }

    /// The open project, cloned rather than borrowed: every caller wants a
    /// path to read a file with, and holding the lock across a filesystem
    /// call would serialise requests behind each other for no reason.
    fn project(&self) -> Option<StudyDesignerConfig> {
        self.0.project.lock().unwrap().clone()
    }

    pub(crate) fn repo_path(&self) -> Option<std::path::PathBuf> {
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
    /// (`embarch-study-designer` decision 52).
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

    /// The firmware repo's own `embarch/protocols/*.eap` — the protocol
    /// manifests a `RunProtocol` step can hand the link to
    /// (`embarch-study-designer` decisions 58-62).
    ///
    /// Read fresh on every call rather than cached, for exactly the reason
    /// [`Self::registry`] and [`Self::structs`] are: these files are edited
    /// beside the running UI — by this tab's own editor dialog, and by hand
    /// — and a cached copy would mean a fix needs a restart to take effect.
    ///
    /// A scan never fails on a bad file (`eap_repo::scan`'s own contract), so
    /// the `Err` here is only "no project" or a directory that could not be
    /// read at all.
    fn protocols(&self) -> Result<RepoProtocols, String> {
        let Some(repo) = self.repo_path() else {
            return Err(NO_PROJECT.to_string());
        };
        embarch_study_designer::eap_repo::scan(&repo).map_err(|e| e.to_string())
    }

    /// Every protocol this repo declares, ready to be resolved against by
    /// `build_study`.
    ///
    /// **A duplicate protocol name is refused here**, not silently resolved
    /// — that refusal is what lets a row name a protocol by name alone, so
    /// it has to reach the caller as an error rather than as a shorter list.
    fn protocol_defs(&self) -> Result<Vec<ProtocolDef>, String> {
        self.protocols()?.defs().map_err(|e| e.to_string())
    }

    /// Runs the configured `static_extractor` at most once per project.
    /// An unrecognized name is a named error the first time it's needed,
    /// not a silent guess — `zephyr-ble-def` is the only name this crate
    /// currently ships an extractor for (`embarch-study-designer` decision 33).
    ///
    /// The failure is *logged* here and swallowed, because every caller of
    /// this is a request that has something else to answer and a repo with a
    /// broken extractor still has an action registry, saved studies and a
    /// live GATT table. [`Self::force_static_extraction`] is the surface that
    /// hands the failure to a human who asked for the extraction itself.
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
        let computed = run_static_extraction(&project)
            .map_err(|e| tracing::warn!("static GATT extraction failed: {e}"))
            .ok()
            .flatten();
        *cached = Some(computed.clone());
        computed
    }

    /// Re-runs the extractor **now**, discarding the cache, and returns what
    /// it found or why it found nothing.
    ///
    /// The cached path above exists because a repo's source tree doesn't
    /// change while a request is being served. It does change while the tab
    /// is open — that is what editing firmware *is* — so an explicit "run the
    /// extractor" button that served a cached answer would be a button that
    /// does nothing after its first press. This is the one entry point that
    /// invalidates first and reports the error rather than logging it.
    fn force_static_extraction(&self) -> Result<Option<StaticGatt>, String> {
        let Some(project) = self.project() else {
            return Err(NO_PROJECT.to_string());
        };
        let computed = run_static_extraction(&project);
        // Cached on failure too, as `None`: the next ordinary request must
        // not silently re-run an extractor that just failed, and the error
        // itself has already been handed to the human who asked.
        *self.0.static_gatt.lock().unwrap() = Some(computed.clone().unwrap_or(None));
        computed
    }

    fn static_gatt(&self) -> Option<Vec<GattServiceInfo>> {
        self.static_extraction().map(|extraction| extraction.services)
    }

    /// Every characteristic name this project can resolve
    /// (`embarch-study-designer` decision 56): the vendor table
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

/// The extraction proper, with its failure returned rather than logged.
///
/// `Ok(None)` and `Err(_)` are different answers and are kept apart all the
/// way to the browser: no extractor configured is the ordinary state of a
/// repo nobody has pointed one at, and an extractor that ran and failed is a
/// fault. Collapsing them is how "static analysis found nothing" came to mean
/// both "there is nothing" and "nobody looked".
fn run_static_extraction(project: &StudyDesignerConfig) -> Result<Option<StaticGatt>, String> {
    match project.static_extractor.as_deref() {
        Some(STATIC_EXTRACTOR_NAME) => ZephyrBleDefExtractor
            .extract_labeled(&project.firmware_repo_path)
            .map(|extracted| {
                Some(StaticGatt {
                    services: extracted.services.iter().cloned().collect(),
                    symbols: extracted.characteristic_symbols().collect(),
                    service_symbols: extracted.service_symbols().collect(),
                })
            })
            .map_err(|e| e.to_string()),
        Some(other) => Err(format!(
            "unrecognized static extractor '{other}' — '{STATIC_EXTRACTOR_NAME}' is the only one \
             this build ships"
        )),
        None => Ok(None),
    }
}

/// The one extractor name this build answers to
/// (`embarch-study-designer` decision 33). Named once because the
/// project panel's placeholder, the refusal text and the match arm all have
/// to agree, and they did not: the refusal said `reference-dut` while the
/// match arm read `zephyr-ble-def`, so the only way to learn the right name
/// was to read this file.
const STATIC_EXTRACTOR_NAME: &str = "zephyr-ble-def";

/// One static extraction, cached per project: the GATT table plus the C
/// identifiers behind it (`embarch-study-designer` decision 56).
#[derive(Debug, Clone)]
struct StaticGatt {
    services: Vec<GattServiceInfo>,
    symbols: Vec<(Uuid, String)>,
    /// The identifiers the *services* were declared under
    /// (`embarch-study-designer` decision 57) — what the
    /// selective-monitor picker's group headers read
    /// (decision 17).
    service_symbols: Vec<(Uuid, String)>,
}

/// Said in one place, because it is both an HTTP body and (via
/// [`StudyDesigner::registry`]) an error string a different layer renders.
const NO_PROJECT: &str = "no project is open — open a firmware repo, or set \
     [study_designer].firmware_repo_path in embarch-ui's config";

/// The answer for a route that needs an open project and hasn't got one.
///
/// Still a `404`, and still the "clear not-configured state" decision 14's
/// predecessor chose over guessing — but it is no longer a dead end, because
/// `POST /api/study-designer/project` is now the way out of it (decision 14).
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
                protocol: None,
                entry_state: None,
            },
            timeout_ms: 15_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        },
        TableRow {
            name: "discover".to_string(),
            action: RowAction::BuiltIn {
                targets: Vec::new(),
                which: BuiltInActionKind::GattDiscover,
                role: RoleChoice::Central,
                target_name: None,
                security_level: None,
                protocol: None,
                entry_state: None,
            },
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
        // No protocol catalogue: a discovery study is two fixed built-in
        // rows, neither of which is a `RunProtocol`, so there is nothing for
        // one to resolve against.
        &[],
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
/// (`embarch-study-designer` decision 40), as the browser stated
/// it — **built 2026-08-26, Milestone 7 Phase D** (decision 11). Until
/// then this function returned `Requirements::any()`
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
    /// The firmware this study builds for itself, when the Build card's
    /// toggle is on (decision 11, reversed). Absent is the common case and
    /// means what it always meant: the DUT is whatever somebody already
    /// flashed.
    #[serde(default)]
    build: Option<BuildSpecInput>,
    /// The outpost trace mode this study needs, authored as flag *names*
    /// rather than as a byte — see [`OutpostModeInput`].
    #[serde(default)]
    outpost: Option<OutpostModeInput>,
}

/// A build spec as the Build card authors it: plain `String`s, converted to
/// the `heapless` shape here so an over-long field is a named refusal
/// rather than a truncation.
///
/// **Snippets arrive as an ordered list and are stored in that order.** The
/// picker is a list an engineer arranges, not a set of checkboxes, because
/// west applies `-S` in order and
/// `embarch-decision-reversals.md` row 109 is a case where the order is the
/// difference between a working image and one whose tracer has no UART.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BuildSpecInput {
    #[serde(default)]
    board: Option<String>,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    snippets: Vec<String>,
    #[serde(default)]
    extra_args: Vec<String>,
}

/// An outpost mode requirement as the Build card authors it: two lists of
/// flag **names**.
///
/// **Names rather than a byte, on this hop only.** The stored
/// `OutpostModeRequirement` is two `u8` masks, which is the right shape for
/// a check; it is the wrong shape for a thing a human types, a thing a
/// reviewer reads in a diff of `requires`, and a thing this file has to
/// report an error about. `HeaderFlags::bit` is the one table both
/// directions go through, so a name this build does not know is a named
/// refusal instead of a silently-zero mask.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutpostModeInput {
    #[serde(default)]
    set: Vec<String>,
    #[serde(default)]
    clear: Vec<String>,
}

impl OutpostModeInput {
    fn mask(names: &[String], which: &str) -> Result<u8, String> {
        let mut mask = 0u8;
        for name in names {
            let bit = embarch_study_designer::outpost::HeaderFlags::bit(name.trim())
                .ok_or_else(|| {
                    let known: Vec<&str> = embarch_study_designer::outpost::HeaderFlags::NAMED
                        .iter()
                        .map(|(_, n)| *n)
                        .collect();
                    format!(
                        "requires.outpost.{which} names '{}', which is not an outpost header                          flag. Known flags: {}",
                        name.trim(),
                        known.join(", ")
                    )
                })?;
            mask |= bit;
        }
        Ok(mask)
    }

    fn build(&self) -> Result<OutpostModeRequirement, String> {
        let requirement = OutpostModeRequirement {
            required_set: Self::mask(&self.set, "set")?,
            required_clear: Self::mask(&self.clear, "clear")?,
        };
        requirement.validate().map_err(|e| e.to_string())?;
        Ok(requirement)
    }
}

impl BuildSpecInput {
    fn build(&self) -> Result<BuildSpec, String> {
        let axis = |raw: &Option<String>, what: &str| -> Result<Option<HString<MAX_BUILD_TARGET_FIELD_LEN>>, String> {
            let Some(raw) = raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                // An empty box in the dialog is "don't narrow on this axis",
                // which is a real answer — the resolver fills it from the
                // project's own `default_target`. Only a *stored* blank is
                // refused, and this is where the two stop being the same
                // thing.
                return Ok(None);
            };
            HString::try_from(raw)
                .map(Some)
                .map_err(|_| too_long(what, raw, MAX_BUILD_TARGET_FIELD_LEN))
        };

        let mut snippets: HVec<HString<MAX_SNIPPET_NAME_LEN>, MAX_SNIPPETS_PER_BUILD> = HVec::new();
        for name in self.snippets.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let name = HString::try_from(name)
                .map_err(|_| too_long("build.snippets entry", name, MAX_SNIPPET_NAME_LEN))?;
            snippets.push(name).map_err(|_| {
                format!(
                    "this build names more than {MAX_SNIPPETS_PER_BUILD} snippets, which is all                      a study can carry"
                )
            })?;
        }

        let mut extra_args: HVec<HString<MAX_BUILD_EXTRA_ARG_LEN>, MAX_BUILD_EXTRA_ARGS> =
            HVec::new();
        for arg in self.extra_args.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let arg = HString::try_from(arg)
                .map_err(|_| too_long("build.extra_args entry", arg, MAX_BUILD_EXTRA_ARG_LEN))?;
            extra_args.push(arg).map_err(|_| {
                format!(
                    "this build names more than {MAX_BUILD_EXTRA_ARGS} extra west flags, which                      is all a study can carry"
                )
            })?;
        }

        let spec = BuildSpec {
            board: axis(&self.board, "build.board")?,
            variant: axis(&self.variant, "build.variant")?,
            revision: axis(&self.revision, "build.revision")?,
            app: axis(&self.app, "build.app")?,
            snippets,
            extra_args,
        };
        spec.validate().map_err(|e| e.to_string())?;
        Ok(spec)
    }
}

fn too_long(what: &str, raw: &str, cap: usize) -> String {
    format!(
        "requires.{what} is {} characters and a study can carry {cap}",
        raw.chars().count()
    )
}

impl RequirementsInput {
    /// `Requirements::any()`, for the one caller that genuinely has no
    /// operator behind it (`discover`, a fixed one-click probe of the DUT's
    /// GATT table that is not an authored study).
    fn any() -> RequirementsInput {
        RequirementsInput {
            dev_bench_version: REQUIREMENT_ANY.to_string(),
            firmware_version: REQUIREMENT_ANY.to_string(),
            build: None,
            outpost: None,
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
            build: self.build.as_ref().map(BuildSpecInput::build).transpose()?,
            outpost: self.outpost.as_ref().map(OutpostModeInput::build).transpose()?,
        };
        requires.validate().map_err(|e| e.to_string())?;
        Ok(requires)
    }
}

/// One tap, as this tab authors it (`embarch-study-designer` decisions
/// 39/52/55, `embarch-outpost` decisions 11/12).
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
/// decode its payloads with** (`embarch-study-designer` decision 52). Undeclared, its file is raw
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
        /// The record magic this capture's frames begin with
        /// (`embarch-study-designer` decision 70). Empty means **no check**,
        /// which is the honest state for a payload whose framing nobody has
        /// declared.
        ///
        /// **The check rides on its tap rather than sitting in a parallel
        /// list**, because it is a fact about *this* capture and because
        /// `build_taps` is already the one place a tap's `id` is assigned —
        /// so the `RecordCheck.stream_id` it mints cannot disagree with the
        /// `StreamTap.id` it is about. A separate authored list would need
        /// its own way to name a tap, which is a second identity for one
        /// thing.
        ///
        /// Bytes, not a string. The magic is a byte run — `GWF1` is ASCII
        /// and `BSS\x03` is not — and the browser parses whichever the
        /// author typed with the same `parseBytes` the registration form
        /// uses. It is **never truncated**: a shortened magic finds
        /// different record boundaries, which is a check that measures the
        /// wrong thing and passes.
        #[serde(default)]
        record_magic: Vec<u8>,
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
) -> Result<(StreamList, DecoderList, RecordCheckList), String> {
    let mut out: StreamList = StreamList::new();
    let mut decoders: DecoderList = DecoderList::new();
    let mut record_checks: RecordCheckList = RecordCheckList::new();

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
            TapInput::GattNotify {
                service_uuid,
                characteristic_uuid,
                decoder,
                record_magic,
                ..
            } => {
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
                // nothing, passes, and looks fine — one instance of the
                // "nothing captured, no error" family `embarch-study-designer`
                // decisions 34/36/54/55 were each opened by; decision 54 names
                // the family, decision 55 describes this exact case. Refused
                // here, where the author can fix it, rather than discovered
                // as an empty file after a run.
                if !any_step_subscribes(steps, service_uuid, characteristic_uuid) {
                    return Err(format!(
                        "stream tap '{name}' captures {}, but no step in this study subscribes \
                         to it — add a GattMonitorAll/GattMonitorStart step, or name it in a \
                         selective monitor step's targets",
                        characteristic_uuid.to_hyphenated()
                    ));
                }

                // Resolved here, against the firmware repo's own
                // `study-structs.toml`, so the submitted `Study` carries the
                // layout rather than a name Core has no way to look up
                // (`embarch-study-designer` decision 52).
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

                // Minted here, with the `id` this loop already assigned, so a
                // check and the tap it is about cannot disagree about which
                // stream they mean.
                if !record_magic.is_empty() {
                    if record_magic.len() > MAX_RECORD_MAGIC_LEN {
                        return Err(format!(
                            "stream tap '{name}': a record magic is {} bytes and the wire \
                             allows {MAX_RECORD_MAGIC_LEN} — a shortened magic would find \
                             different record boundaries, so it is refused rather than cut",
                            record_magic.len()
                        ));
                    }
                    let magic = HVec::from_slice(record_magic).map_err(|_| {
                        format!("stream tap '{name}': record magic does not fit the wire")
                    })?;
                    record_checks
                        .push(RecordCheck {
                            stream_id: id,
                            framing: RecordFraming::MagicPrefixedCrc32Le { magic },
                        })
                        .map_err(|_| {
                            format!("more than {MAX_STREAMS_PER_STUDY} record checks in one study")
                        })?;
                }

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
    Ok((out, decoders, record_checks))
}

/// Whether any step in `steps` subscribes to this characteristic — an
/// unfiltered monitor action (which subscribes to everything notify- or
/// indicate-capable) or a selective one that names it
/// (`embarch-study-designer` decision 53).
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
/// after whatever the author declared — decision 15.
///
/// **Auto-declared rather than offered as a checkbox.** Before this, the
/// Study Designer authored no GATT tap at all, so a monitor step's capture
/// existed only as `StepResult.gatt_activity`'s first 32 records — and that
/// field is retired (`embarch-study-designer` decision 54). A
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

/// Every submitter recomputes **all three** of a study's seals immediately
/// before sending (`embarch-study-designer` decision 26) — the same three
/// lines `embarch-api`'s own `study.rs::reseal_study` uses, inlined here
/// rather than depending on that crate for them.
///
/// The three, and why each is here rather than assumed:
///
/// - `steps_crc` over `Study.steps`, the original seal.
/// - `streams_crc` over `Study.streams`, `embarch-study-designer` decision
///   39's 2026-08-25 amendment's sibling seal. This tab authors taps, so it
///   is routinely non-zero.
/// - `protocols_crc` over `Study.protocols`, decision 58's third seal. A
///   study built from this tab's rows carries protocols only once a
///   `RunProtocol` row names one; before that it reseals to the empty-list
///   value — which is genuinely 0, not a placeholder — but it is computed
///   rather than assumed.
///
/// **The third line was missing until 2026-09-17**, which is the same
/// hardcoded-arity trap `embarch-api`'s `reseal_study` hit on 2026-08-27
/// ([embarch-decision-reversals.md] row 76): a seal added as a deliberate
/// *sibling* has to be added everywhere the set is enumerated, and "both" in
/// a doc comment is the kind of arity that silently becomes false. Nothing
/// was observable while `Study.protocols` was always empty — the first
/// protocol-carrying study would have been `400`ed by Core's third seal
/// check.
fn seal_crc(study: &mut Study) -> Result<(), String> {
    study.steps_crc = embarch_study_designer::steps_crc(&study.steps).map_err(|e| format!("{e:?}"))?;
    study.streams_crc =
        embarch_study_designer::streams_crc(&study.streams).map_err(|e| format!("{e:?}"))?;
    study.protocols_crc =
        embarch_study_designer::protocols_crc(&study.protocols).map_err(|e| format!("{e:?}"))?;
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
    /// (`embarch-study-designer` decisions 53/55).
    ///
    /// A separate list from `actions` rather than a filter over it: `actions`
    /// is keyed by characteristic for *authoring a write*, and its `Vendor`
    /// entries are a compile-time table with no observed properties byte at
    /// all. Whether a characteristic can notify is an observation, and this
    /// list carries only observations.
    subscribable: Vec<SubscribableCharacteristic>,
    /// The firmware repo's `embarch/study-structs.toml` entries — what a
    /// `GattNotify` tap's decoder dropdown offers (`embarch-study-designer`
    /// decision 52) and what the layout editor edits. Empty when the repo
    /// declares none, which is the ordinary starting state.
    struct_layouts: Vec<StructLayoutSummary>,
    /// What every picker that names a characteristic labels its options with
    /// (`embarch-study-designer` decision 56), keyed by
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
    /// (`embarch-study-designer` decision 57). What the
    /// selective-monitor picker's group headers read
    /// (decision 17) — a picker that groups by
    /// service needs a name for the group, and `sds_service` is a heading an
    /// engineer can navigate by where `00000001` is not.
    service_names: BTreeMap<String, GattName>,
    /// `limits::MAX_MONITOR_TARGETS` — how many characteristics one selective
    /// monitor step may name (`embarch-study-designer` decision 53).
    /// Served rather than restated in `app.js`: the cap is enforced by
    /// `build_study`, and a browser-side copy of it is a number that drifts
    /// silently the day the limit moves. The picker (decision 17) shows it
    /// and stops at it, so
    /// the refusal happens where the choice is made rather than as a `400`
    /// after a round trip.
    max_monitor_targets: usize,
    /// `limits::MAX_STREAM_NAME_LEN` — how long a `StreamTap`/`StreamRef`
    /// name may be (`embarch-study-designer/interfaces/result-types.md`), also the file
    /// name a stream becomes under a study's `streams/` directory. Served
    /// rather than restated in `app.js`: the cap is enforced by
    /// `build_study`, same reasoning as [`Self::max_monitor_targets`] above.
    /// The tap-naming default (`initSdTaps`'s `sd-add-gatt-tap` handler)
    /// slices the characteristic label to this length rather than to a
    /// literal `32`.
    max_stream_name_len: usize,
    /// Every protocol this repo's `embarch/protocols/*.eap` files declare
    /// that **resolved** — what a `RunProtocol` row's protocol picker offers
    /// and what its entry-state picker reads its states from
    /// (`embarch-study-designer` decisions 58-62).
    ///
    /// A file that did not parse contributes no entry here; it is reported
    /// by the editor's own `GET /protocols`, which is where a file's errors
    /// belong. This list answers "what can a study reach".
    protocols: Vec<ProtocolSummary>,
    /// `limits::MAX_PROTOCOLS_PER_STUDY` — how many distinct protocols one
    /// study's rows may name between them. Served for the reason every other
    /// cap here is: `build_study` enforces it, and a browser-side copy
    /// drifts silently the day it moves.
    max_protocols_per_study: usize,
    /// `limits::MAX_RECORD_MAGIC_LEN` — the longest record magic a tap may
    /// declare (`embarch-study-designer` decision 70).
    max_record_magic_len: usize,
    /// `limits::MAX_STRUCT_FIELDS` — scalars per group in one payload
    /// layout, which is what the layout editor stops at.
    max_struct_fields: usize,
    /// `decoder::ScalarType::ALL`'s spellings, in picker order — the layout
    /// editor's type dropdown.
    ///
    /// **An empty list is an empty picker and a refusal**, never a guessed
    /// eighteen: the browser has no fallback for a served vocabulary, the
    /// same posture it takes for `max_stream_name_len`.
    scalar_types: Vec<&'static str>,
    /// `DevBenchLogLevel::ALL`, each as the JSON spelling a saved study
    /// carries plus the label naming what choosing it costs.
    dev_bench_log_levels: Vec<LogLevelOption>,
    /// Stems of `.eap` files that did not parse at all.
    ///
    /// A file that did not parse declares nothing this can name, so a row
    /// pointing into one cannot be told apart from a row pointing at a
    /// deleted protocol — except by saying that some file in this repo is
    /// unreadable, which is what this list is for.
    unparsed_files: Vec<String>,
    /// The three advisory dev-bench caps
    /// (`embarch-study-designer::limits`'s advisory band).
    ///
    /// **Advisory, never a gate.** They are what the caps note reads; Run is
    /// never disabled by one. Absent — which is what a browser sees when
    /// this response could not be built — renders as *unknown*, never as
    /// "within caps".
    dev_bench_limits: DevBenchLimits,
}

/// One resolved `.eap` protocol, as the row pickers render it.
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolSummary {
    /// The `protocol <name> { … }` identifier — what a row carries.
    name: String,
    /// The file stem it is declared in, so the editor can be opened at the
    /// right file. A row never carries this: the name alone resolves,
    /// because `eap_repo::defs` refuses a repo declaring one name twice.
    file: String,
    /// False for a block that **parsed but did not resolve** — its name is
    /// known and its states are not.
    ///
    /// Listed rather than omitted, because the two are different facts and a
    /// row naming it should be able to say which. A row pointing at an
    /// unresolved block is not a row pointing at nothing: the protocol is
    /// there, in a file, with something wrong inside it — and "no states"
    /// would be a claim about the manifest where "unknown until it resolves"
    /// is a statement about our ability to read it.
    ///
    /// A study naming one is still refused by `build_study`, which resolves
    /// against `defs()` and sees only resolved blocks.
    resolved: bool,
    /// Empty whenever `resolved` is false.
    states: Vec<ProtocolStateSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProtocolStateSummary {
    name: String,
    /// True for a state a run ends in. The entry-state picker refuses one
    /// **before submit** from this flag — a terminal entry state would pass
    /// instantly and capture nothing.
    terminal: bool,
    /// `"pass"` or `"fail"` for a terminal state, absent otherwise. Rendered
    /// verbatim; nothing in the browser reasons about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLevelOption {
    /// The JSON spelling — bare PascalCase, which is what a saved study
    /// carries and what this route accepts back.
    value: String,
    label: &'static str,
    /// What choosing this level costs, as prose the picker shows under it.
    /// Empty for a level with nothing to warn about.
    ///
    /// **Served rather than written in `app.js`** for the reason every other
    /// vocabulary here is: prose keyed on a level name is a browser-side
    /// copy of the level set, and it goes stale the same way a label does.
    note: &'static str,
    /// True for the level an unstated study runs at.
    ///
    /// Served because *which* level is the default is a fact of
    /// `DevBenchLogLevel`, not of this browser. A browser that pre-selected
    /// `Warn` by name would be asserting that default from a second place —
    /// and a picker that pre-selected nothing would show `Off` first, which
    /// is the one level a study must never reach by not choosing.
    default: bool,
}

/// The prose under each level in the picker.
///
/// Distinct from [`DevBenchLogLevel::label`], which is the one-line option
/// text and belongs to the crate. This is UI prose about what the choice
/// costs an author, so it lives beside the route that serves it.
fn log_level_note(level: DevBenchLogLevel) -> &'static str {
    match level {
        DevBenchLogLevel::Off => {
            "Nothing comes back — not even the fatal-error dump, so a crash mid-study goes \
             unreported. The level for a study that needs a clear link and is willing to be \
             blind."
        }
        DevBenchLogLevel::Info | DevBenchLogLevel::Debug => {
            "The bench clamps this to whatever its firmware was actually built with, and \
             reports the clamp on its own dev-bench log stream — read it there after the run \
             rather than assuming this is what it used. Expect real link bandwidth during \
             BLE-heavy steps."
        }
        DevBenchLogLevel::Error | DevBenchLogLevel::Warn => "",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DevBenchLimits {
    max_steps_per_study: usize,
    max_event_arms_per_state: usize,
    max_protocols_wire_len: usize,
}

/// One payload layout, with its fields — not just its name.
///
/// **Widened from the bare `Vec<String>` this shipped with** because the
/// layout editor has to render what an existing layout *is* before it can
/// edit it, and the tap's decoder dropdown still only needs `name`. One
/// shape read by both, rather than a second route serving the same file.
#[derive(Debug, Clone, Serialize)]
pub struct StructLayoutSummary {
    name: String,
    header: Vec<StructFieldSummary>,
    repeat: Vec<StructFieldSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StructFieldSummary {
    name: String,
    #[serde(rename = "type")]
    ty: String,
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
/// found (`embarch-study-designer` decision 56). A characteristic
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

/// The same, for services (`embarch-study-designer` decision
/// 56). Separate from `characteristic_names` because the lookup is: a
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
        Ok(r) => r.structs.into_iter().map(struct_layout_summary).collect(),
        Err(e) => {
            tracing::warn!("study-structs.toml could not be read: {e}");
            Vec::new()
        }
    };
    // Same posture, one directory over: a repo whose `.eap` files are
    // mid-edit still gets a usable action list. `scan` never fails on a bad
    // file, so what lands here is "no project" or an unreadable directory —
    // and either way this list answers "what can a study reach", with the
    // per-file errors belonging to `GET /protocols`.
    //
    // A duplicate protocol name across files is deliberately NOT refused
    // here: this is a picker, and refusing to render one would leave an
    // author with no way to see which two files collide. The refusal happens
    // where it matters, when a study is built.
    let (protocols, unparsed) = match sd.protocols() {
        Ok(repo) => (protocol_summaries(&repo), unparsed_files(&repo)),
        Err(e) => {
            tracing::warn!("embarch/protocols could not be read: {e}");
            (Vec::new(), Vec::new())
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
        max_stream_name_len: MAX_STREAM_NAME_LEN,
        max_protocols_per_study: embarch_study_designer::limits::MAX_PROTOCOLS_PER_STUDY,
        max_record_magic_len: MAX_RECORD_MAGIC_LEN,
        max_struct_fields: embarch_study_designer::limits::MAX_STRUCT_FIELDS,
        scalar_types: embarch_study_designer::ScalarType::ALL.iter().map(|t| t.as_str()).collect(),
        dev_bench_log_levels: DevBenchLogLevel::ALL
            .iter()
            .map(|l| LogLevelOption {
                // Through serde rather than a hand-written string: this is
                // the spelling a saved study carries, and the one place it
                // is defined is that impl.
                value: serde_json::to_value(l)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                label: l.label(),
                note: log_level_note(*l),
                default: *l == DevBenchLogLevel::default(),
            })
            .collect(),
        dev_bench_limits: DevBenchLimits {
            max_steps_per_study: embarch_study_designer::limits::DEV_BENCH_MAX_STEPS_PER_STUDY,
            max_event_arms_per_state:
                embarch_study_designer::limits::DEV_BENCH_MAX_EVENT_ARMS_PER_STATE,
            max_protocols_wire_len:
                embarch_study_designer::limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN,
        },
        protocols,
        unparsed_files: unparsed,
        actions,
        live_gatt_available: live.is_some(),
        static_gatt_available: static_gatt.is_some(),
        struct_layouts,
    })
    .into_response()
}

/// One `study-structs.toml` entry as the response carries it — the TOML's own
/// `StructDef`, renamed rather than re-derived, so a field spelling this
/// serves is the spelling that file holds.
fn struct_layout_summary(
    def: embarch_study_designer::registry::StructDef,
) -> StructLayoutSummary {
    let field = |f: embarch_study_designer::registry::StructFieldDef| StructFieldSummary {
        name: f.name,
        ty: f.ty,
    };
    StructLayoutSummary {
        name: def.name,
        header: def.header.into_iter().map(field).collect(),
        repeat: def.repeat.into_iter().map(field).collect(),
    }
}

/// Every resolved protocol in a scanned repo, with its states.
///
/// Only resolved blocks: a file that did not parse contributes nothing here
/// and is reported by `GET /protocols` instead, where a file's own errors
/// belong. Duplicated names across files appear twice on purpose — this is a
/// picker, and hiding one would leave an author unable to see the collision
/// that `build_study` will refuse.
fn protocol_summaries(repo: &RepoProtocols) -> Vec<ProtocolSummary> {
    let mut out = Vec::new();
    for file in &repo.files {
        for block in &file.blocks {
            match &block.resolved {
                Ok(resolved) => out.push(ProtocolSummary {
                    name: block.name.clone(),
                    file: file.stem.clone(),
                    resolved: true,
                    states: state_summaries(resolved),
                }),
                Err(_) => out.push(ProtocolSummary {
                    name: block.name.clone(),
                    file: file.stem.clone(),
                    resolved: false,
                    states: Vec::new(),
                }),
            }
        }
    }
    out
}

/// Stems of files that did not parse at all. See
/// [`ActionsResponse::unparsed_files`].
fn unparsed_files(repo: &RepoProtocols) -> Vec<String> {
    repo.files
        .iter()
        .filter(|f| f.parsed.is_err())
        .map(|f| f.stem.clone())
        .collect()
}

/// One resolved protocol's states, as every picker and the editor render
/// them. One function, so the editor's summary and the row's entry-state
/// dropdown cannot describe the same protocol differently.
fn state_summaries(
    resolved: &embarch_study_designer::ResolvedProtocol,
) -> Vec<ProtocolStateSummary> {
    resolved
        .def
        .states
        .iter()
        .map(|state| ProtocolStateSummary {
            name: state.name.to_string(),
            terminal: matches!(state.kind, embarch_study_designer::StateKind::Terminal(_)),
            outcome: match state.kind {
                embarch_study_designer::StateKind::Terminal(
                    embarch_study_designer::TerminalOutcome::Pass,
                ) => Some("pass"),
                embarch_study_designer::StateKind::Terminal(
                    embarch_study_designer::TerminalOutcome::Fail,
                ) => Some("fail"),
                _ => None,
            },
        })
        .collect()
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

/// A registration, a rename, or an edit — one shape, because from a form's
/// point of view they are one operation with one optional extra field.
///
/// `previous_name` absent is today's upsert, unchanged. Present and
/// different makes this a rename: the old name is reference-checked first,
/// because a rename is a delete as far as a saved study naming it is
/// concerned.
#[derive(Debug, Deserialize)]
pub struct RegisterActionRequest {
    #[serde(default)]
    previous_name: Option<String>,
    #[serde(flatten)]
    action: RegisteredAction,
}

/// Registers, edits or renames one `RegisteredAction` — never a semantic
/// "what does this do" field anywhere on this type
/// (`embarch-study-designer` decision 35's own non-goal).
///
/// **Both registries' `save` rewrite the whole TOML through
/// `to_string_pretty`, so comments in a hand-edited file are lost.** Already
/// true of the upsert this grew out of; an edit form makes it routine. Said
/// here, and said in the dialog. A comment-preserving writer is a new
/// dependency and its own decision.
pub async fn api_register_action(
    State(state): State<crate::AppState>,
    Json(req): Json<RegisterActionRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let mut registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let RegisterActionRequest { previous_name, action } = req;

    if let Some(previous) = previous_name.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        if previous != action.name {
            let scan = scan_references(&project, RefKind::Action, previous);
            if scan.blocks() {
                return refusal("registered action", previous, &scan);
            }
            registry.actions.retain(|a| a.name != previous);
        }
    }

    registry.actions.retain(|a| a.name != action.name);
    registry.actions.push(action);
    match registry.save(&project.firmware_repo_path) {
        Ok(()) => Json(registry).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// Deletes one registered action, refusing while a saved study still names
/// it.
pub async fn api_registry_delete(
    State(state): State<crate::AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let mut registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    if !registry.actions.iter().any(|a| a.name == name) {
        return (StatusCode::NOT_FOUND, format!("no registered action named '{name}'"))
            .into_response();
    }
    let scan = scan_references(&project, RefKind::Action, &name);
    if scan.blocks() {
        return refusal("registered action", &name, &scan);
    }
    registry.actions.retain(|a| a.name != name);
    match registry.save(&project.firmware_repo_path) {
        Ok(()) => Json(registry).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

// ---- payload layouts (`embarch-study-designer` decision 52) --------------
//
// `study-structs.toml` had no write route at all: a layout could only be
// added by hand-editing the file beside the running UI. These three are the
// action registry's routes one file over, with the same reference-check
// posture — and every write goes through `StructRegistry::save`, so
// validation and file layout stay the crate's rather than becoming a second
// implementation here.

pub async fn api_structs(State(state): State<crate::AppState>) -> axum::response::Response {
    let sd = state.study_designer;
    if sd.project().is_none() {
        return not_configured();
    }
    match sd.structs() {
        Ok(r) => Json(r).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// A layout registration, edit or rename — `RegisterActionRequest`'s
/// counterpart, deliberately the same shape so the two dialogs are the same
/// dialog with a different body.
#[derive(Debug, Deserialize)]
pub struct StructRequest {
    #[serde(default)]
    previous_name: Option<String>,
    #[serde(flatten)]
    layout: embarch_study_designer::registry::StructDef,
}

pub async fn api_struct_save(
    State(state): State<crate::AppState>,
    Json(req): Json<StructRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let mut registry = match sd.structs() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let StructRequest { previous_name, layout } = req;

    if let Some(previous) = previous_name.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        if previous != layout.name {
            let scan = scan_references(&project, RefKind::Layout, previous);
            if scan.blocks() {
                return refusal("payload layout", previous, &scan);
            }
            registry.structs.retain(|d| d.name != previous);
        }
    }

    registry.structs.retain(|d| d.name != layout.name);
    registry.structs.push(layout);
    // Through the crate's own `save`, which validates first — a layout whose
    // field type is `u24le` or whose name is too long is refused there, in
    // the one place that refusal is written.
    match registry.save(&project.firmware_repo_path) {
        Ok(()) => Json(registry).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

pub async fn api_struct_delete(
    State(state): State<crate::AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let mut registry = match sd.structs() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    if !registry.structs.iter().any(|d| d.name == name) {
        return (StatusCode::NOT_FOUND, format!("no payload layout named '{name}'"))
            .into_response();
    }
    let scan = scan_references(&project, RefKind::Layout, &name);
    if scan.blocks() {
        return refusal("payload layout", &name, &scan);
    }
    registry.structs.retain(|d| d.name != name);
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
    // implied: this UI never builds or flashes anything (decision 5's
    // amendment routes every hardware-adjacent operation through
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
    /// (`embarch-study-designer` decision 40). Ticked in the run
    /// dialog against the actual discrepancy, which decision 11 is explicit
    /// about: the mismatch is shown *before* the run, with both strings, so
    /// the choice is made against the real gap rather than in the abstract.
    ///
    /// Never silently honoured — Core records it in
    /// `StudyResult.provenance.overrides` with both strings, and this tab
    /// renders that.
    #[serde(default)]
    allow_version_mismatch: bool,
    /// How loud dev-bench's firmware should be for this run
    /// (`embarch-dev-bench` decision 39). Absent leaves the crate's own
    /// default (`Warn`) in place — see `build_authored`, which is the one
    /// place that decision is read.
    ///
    /// A property of the **saved study**, not of the run: decision 51
    /// rejected making it a property of the log tap, so it is saved and
    /// loaded with the study exactly as `requires` is, and it is not a run
    /// dialog field the way `reflash` and `allow_version_mismatch` are.
    #[serde(default)]
    dev_bench_log_level: Option<DevBenchLogLevel>,
}

/// Builds, taps, and seals one authored study — everything `run` and `save`
/// do identically, so a saved file and a submitted study can never disagree
/// about what the rows meant.
#[allow(clippy::too_many_arguments)]
fn build_authored(
    req_name: &str,
    rows: &[TableRow],
    requires: &RequirementsInput,
    taps: &[TapInput],
    registry: &ActionRegistry,
    structs: &StructRegistry,
    protocols: &[ProtocolDef],
    log_level: Option<DevBenchLogLevel>,
) -> Result<Study, (StatusCode, String)> {
    let requires = requires.build().map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let mut study = build_study(req_name, requires, rows, registry, protocols)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    // Taps are built against the *resolved* steps rather than the raw rows:
    // whether a characteristic is subscribed at all is a property of the
    // `Action` a row became, not of the row's own text.
    let (mut streams, decoders, record_checks) = build_taps(taps, &study.steps, structs)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    auto_transcript_tap(&mut streams, &study.steps)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    study.streams = streams;
    study.decoders = decoders;
    study.record_checks = record_checks;
    // **Only when the request said so.** `None` leaves the crate's own
    // argued default (`Warn`) in place rather than this layer restating it:
    // `build_study` gives a whole paragraph to why an unstated level has a
    // correct answer where an unstated `requires` has only a permissive one,
    // and writing `Warn` here would be a second copy of that decision that
    // stops tracking the first.
    if let Some(level) = log_level {
        study.dev_bench_log_level = level;
    }
    seal_crc(&mut study).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(study)
}

pub async fn api_run(
    State(state): State<crate::AppState>,
    Json(req): Json<RunRequest>,
) -> axum::response::Response {
    let sd = state.study_designer.clone();
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
    // A repo whose `.eap` files declare one protocol name twice is refused
    // here rather than resolved by directory order — see `sd.protocol_defs`.
    // `400` and not `500`: the repo's own files are what is wrong, and the
    // person who can fix them is the one looking at this tab.
    let protocols = match sd.protocol_defs() {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let study = match build_authored(
        &req.name,
        &req.rows,
        &req.requires,
        &req.taps,
        &registry,
        &structs,
        &protocols,
        req.dev_bench_log_level,
    ) {
        Ok(s) => s,
        Err((code, e)) => return (code, e).into_response(),
    };
    // **No slug**: this study is the table as it stands, which may never
    // have been saved. A build still runs and still reports what it
    // flashed; there is simply no file to write the built version back
    // into, and inventing one would be saving a study nobody asked to save.
    submit_run(&state, study, req.allow_version_mismatch, None).await
}


/// Submits a study, **building and flashing its DUT firmware first when it
/// declares one** (decision 11, reversed).
///
/// Both run routes go through here, for the reason `run_saved_study`'s own
/// comment already gives about itself: two routes post a study, and one of
/// them had to not be a second implementation.
///
/// # Two different response shapes, and why
///
/// Without a build spec this is exactly what it always was — a `post_study`
/// and a `{ "study_id": … }`, synchronous, unchanged.
///
/// With one, the work is tens of seconds of `west` and this returns
/// `{ "build_id": … }` **immediately**, with the build running in a
/// background task publishing to `GET /api/build/events`. Not because a long
/// request is untidy: a build the browser cannot watch is a spinner, and the
/// owner asked for the log to be a phase of the run, which it can only be if
/// it arrives while it is happening. The browser follows the stream and
/// picks up the study id from its terminal frame.
///
/// # What happens to the study's own `firmware_version`
///
/// **Rewritten on the saved file only when the engineer waved a mismatch
/// through, and never over an explicit `any`.**
///
/// The build refuses before it starts when the working tree is not at the
/// revision the study requires, so in the ordinary case the two already
/// agree and there is nothing to write. The case that is left is the one
/// worth automating: a study pinned to a revision the tree has moved past,
/// run anyway on purpose. Writing the built version back is what stops that
/// study asking the same stale question tomorrow.
///
/// `any` is exempt, and that is not a special case bolted on — it is the
/// same rule decision 11 states for the checkbox. `any` is a deliberate
/// statement that the build does not matter, and silently converting it
/// into a pin would erase a distinction the whole of decision 40 rests on.
async fn submit_run(
    state: &crate::AppState,
    study: Study,
    allow_version_mismatch: bool,
    saved_slug: Option<String>,
) -> axum::response::Response {
    let Some(spec) = study.requires.build.clone() else {
        return post_and_hand_off(state, study, allow_version_mismatch, None).await;
    };

    let sd = state.study_designer.clone();
    let Some(repo) = sd.repo_path() else { return not_configured() };

    let run = state.build_runs.start();
    let build_id = run.id.clone();

    let state = state.clone();
    tokio::spawn(async move {
        let required = study.requires.firmware_version.as_str().to_string();
        run.emit(serde_json::json!({ "kind": "phase", "phase": "build" }));

        let emitter = {
            let run = run.clone();
            std::sync::Arc::new(move |stream: &str, text: &str| {
                run.emit(serde_json::json!({
                    "kind": "line",
                    "stream": stream,
                    "text": text,
                }));
            })
        };

        let flashed = crate::firmware_build::build_and_flash(
            &sd.0.core,
            &state.build_locks,
            state.build_config_path.as_deref(),
            &repo,
            &spec,
            &required,
            allow_version_mismatch,
            emitter,
        )
        .await;

        let flashed = match flashed {
            Ok(f) => f,
            Err(e) => {
                run.finish(serde_json::json!({
                    "kind": "failed",
                    "phase": "build",
                    "error": format!("{e:#}"),
                }));
                return;
            }
        };

        run.emit(serde_json::json!({
            "kind": "flashed",
            "version": flashed.version,
            "artifact_path": flashed.artifact_path,
            "descriptor": flashed.descriptor,
            "log_id": flashed.log_id,
        }));

        let mut study = study;
        let rewrote = match update_saved_firmware_version(
            &sd,
            saved_slug.as_deref(),
            &required,
            &flashed.version,
        ) {
            Ok(rewrote) => rewrote,
            Err(e) => {
                // A study that could not be rewritten is not a run that
                // failed. Said out loud rather than swallowed: the engineer
                // is about to see a run whose requirement still names the
                // old revision.
                run.emit(serde_json::json!({
                    "kind": "line",
                    "stream": "info",
                    "text": format!("the saved study's firmware_version was not updated: {e:#}"),
                }));
                false
            }
        };
        if rewrote {
            // The study about to be posted carries the same value the file
            // now does, so the result's provenance and the file agree.
            if let Ok(v) = HString::try_from(flashed.version.as_str()) {
                study.requires.firmware_version = v;
            }
            run.emit(serde_json::json!({
                "kind": "line",
                "stream": "info",
                "text": format!(
                    "this study required '{required}' and now requires '{}' — the saved file \
                     was updated to what was just flashed",
                    flashed.version
                ),
            }));
        }

        run.emit(serde_json::json!({ "kind": "phase", "phase": "submit" }));
        let posted = post_study_only(
            &sd.0.core,
            &study,
            allow_version_mismatch,
            Some(flashed.version.clone()),
        )
        .await;

        match posted {
            Ok(study_id) => {
                state.live.ensure(&study_id);
                run.finish(serde_json::json!({ "kind": "started", "study_id": study_id }));
            }
            Err(e) => run.finish(serde_json::json!({
                "kind": "failed",
                "phase": "submit",
                "error": e,
            })),
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "build_id": build_id })),
    )
        .into_response()
}

/// `post_study` plus the live-session hand-off, for a run with no build
/// phase in front of it.
async fn post_and_hand_off(
    state: &crate::AppState,
    study: Study,
    allow_version_mismatch: bool,
    flashed: Option<String>,
) -> axum::response::Response {
    let sd = state.study_designer.clone();
    match post_study_only(&sd.0.core, &study, allow_version_mismatch, flashed).await {
        Ok(study_id) => {
            // Registering a live session subscribes embarch-ui once to
            // Core's event stream for this study and starts the rings the
            // Live Study tab reads. The tab needs nothing else to attach.
            state.live.ensure(&study_id);
            Json(serde_json::json!({ "study_id": study_id })).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

async fn post_study_only(
    core: &CoreClient,
    study: &Study,
    allow_version_mismatch: bool,
    flashed_firmware_version: Option<String>,
) -> Result<String, String> {
    let options = StudyRunOptions {
        allow_version_mismatch,
        // **Set only when this process genuinely flashed the board.** Until
        // 2026-09-18 it was unconditionally `None` and the comment said why:
        // this UI built nothing, so claiming otherwise would have turned
        // `VersionSource::FlashedThisRun` from a fact into an assertion. It
        // now builds, so on that path the claim is a fact — and on every
        // other path it is still `None`, for the original reason.
        flashed_firmware_version,
    };
    core.post_study(study, &options)
        .await
        .map(|resp| resp.study_id)
        .map_err(|e| format!("{e:#}"))
}

/// Writes `built` into a saved study's `requires.firmware_version`, when the
/// rules in [`submit_run`]'s doc comment say to. Returns whether it wrote.
///
/// **A whole-value edit of the parsed JSON, not a re-serialization of the
/// `Study`.** The file also carries `_embarch_ui_rows` and `_embarch_ui_taps`
/// — the authoring sidecars that make it loadable back into the table — and
/// rewriting it from the runnable `Study` alone would silently turn an
/// editable study into one the editor refuses with a `409`. None of the
/// three CRCs cover `requires`, so nothing needs resealing.
fn update_saved_firmware_version(
    sd: &StudyDesigner,
    slug: Option<&str>,
    required: &str,
    built: &str,
) -> Result<bool, String> {
    let Some(slug) = slug else { return Ok(false) };
    if required == REQUIREMENT_ANY || required == built {
        return Ok(false);
    }
    let Some(project) = sd.project() else { return Ok(false) };
    let slug = study_slug(slug)?;
    let path = studies_dir(&project).join(format!("{slug}.json"));

    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} isn't valid JSON: {e}", path.display()))?;
    let Some(requires) = value.get_mut("requires").and_then(|r| r.as_object_mut()) else {
        return Err(format!("{} has no `requires` object to update", path.display()));
    };
    requires.insert(
        "firmware_version".to_string(),
        serde_json::Value::String(built.to_string()),
    );

    let rendered = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("couldn't re-render {}: {e}", path.display()))?;
    std::fs::write(&path, format!("{rendered}\n"))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    Ok(true)
}

/// What a pre-flight found. **Never gates**: every field is a reading, and
/// the route answers `200` for a study that is over every advisory cap.
#[derive(Debug, Serialize)]
struct PreflightResponse {
    steps: usize,
    taps: usize,
    decoders: usize,
    record_checks: usize,
    /// The protocols this study's rows actually named, in the order the
    /// study carries them — derived by `build_study`, not authored.
    protocols: Vec<String>,
    /// The postcard-encoded size of `Study.protocols`, length prefix
    /// included — the one advisory dev-bench cap that mirrors no count and
    /// so cannot be checked by counting anything.
    protocols_wire_len: usize,
    dev_bench_log_level: String,
    /// One sentence per advisory cap this study is over.
    ///
    /// **Built here, not in the browser.** Two of the three cannot be
    /// computed there at all: the wire length is a postcard encoding, and
    /// the event-arm count is a property of a resolved `ProtocolDef` the
    /// browser only ever sees a summary of. Building the third here too
    /// keeps them one list with one voice.
    advisories: Vec<String>,
}

/// Builds the study a `run` would submit and reports what it is, without
/// submitting it.
///
/// Same `build_authored` the run and the save take, so a pre-flight cannot
/// describe a different study from the one that would run. A build error is
/// a `400` with the same message the run would have given.
pub async fn api_preflight(
    State(state): State<crate::AppState>,
    Json(req): Json<RunRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
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
    let protocols = match sd.protocol_defs() {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let study = match build_authored(
        &req.name,
        &req.rows,
        &req.requires,
        &req.taps,
        &registry,
        &structs,
        &protocols,
        req.dev_bench_log_level,
    ) {
        Ok(s) => s,
        Err((code, e)) => return (code, e).into_response(),
    };

    let wire_len = match embarch_study_designer::protocols_wire_len(&study.protocols) {
        Ok(len) => len,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")).into_response(),
    };
    Json(PreflightResponse {
        steps: study.steps.len(),
        taps: study.streams.len(),
        decoders: study.decoders.len(),
        record_checks: study.record_checks.len(),
        protocols: study.protocols.iter().map(|p| p.name.to_string()).collect(),
        protocols_wire_len: wire_len,
        dev_bench_log_level: serde_json::to_value(study.dev_bench_log_level)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        advisories: advisories_for(&study, wire_len),
    })
    .into_response()
}

/// One sentence per advisory dev-bench cap this study is over.
///
/// **Advisory, never a gate.** Each sentence says what this study is and
/// what this suite's bench takes, and says nothing about whether to run it:
/// the bench on somebody's desk may be a different build, and a host that
/// refused to run a study its own bench would accept would be enforcing a
/// number it cannot see.
fn advisories_for(study: &Study, protocols_wire_len: usize) -> Vec<String> {
    use embarch_study_designer::limits;
    let mut out = Vec::new();

    if study.steps.len() > limits::DEV_BENCH_MAX_STEPS_PER_STUDY {
        out.push(format!(
            "{} steps — this suite's dev-bench refuses a study over {} at decode",
            study.steps.len(),
            limits::DEV_BENCH_MAX_STEPS_PER_STUDY
        ));
    }
    if protocols_wire_len > limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN {
        out.push(format!(
            "the protocols this study carries encode to {protocols_wire_len} bytes — this \
             suite's dev-bench accepts up to {}",
            limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN
        ));
    }
    // Per state, not per protocol: the cap is on one state's arms, and a
    // protocol with one fat state and five thin ones is over it. Named by
    // protocol and state so the sentence points at the `.eap` to edit.
    for protocol in study.protocols.iter() {
        for state in protocol.states.iter() {
            let embarch_study_designer::StateKind::Active(active) = &state.kind else { continue };
            if active.on_event.len() > limits::DEV_BENCH_MAX_EVENT_ARMS_PER_STATE {
                out.push(format!(
                    "protocol '{}' state '{}' has {} event arms — this suite's dev-bench \
                     accepts {} per state",
                    protocol.name,
                    state.name,
                    active.on_event.len(),
                    limits::DEV_BENCH_MAX_EVENT_ARMS_PER_STATE
                ));
            }
        }
    }
    out
}

/// What a saved study *is*, without loading it into the table.
///
/// Answers for a run-only file — one written by hand or by an agent, which
/// `api_studies_load` refuses with a `409` because it has no
/// `_embarch_ui_rows` to load back. That `409` is unchanged and correct;
/// this is the read-only view the browser shows instead, and it is also
/// what the existing version-check dialog reads, so that dialog needs no
/// route of its own.
#[derive(Debug, Serialize)]
struct StudySummary {
    slug: String,
    name: String,
    editable: bool,
    requires: RequirementsOut,
    dev_bench_log_level: String,
    /// One label per step, in order — enough to read what the study does.
    steps: Vec<String>,
    taps: Vec<String>,
    protocols: Vec<String>,
    record_checks: usize,
}

pub async fn api_study_summary(
    State(state): State<crate::AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    let slug = match study_slug(&slug) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let value = match read_saved_study(&project, &slug) {
        Ok(v) => v,
        Err(response) => return *response,
    };

    let list = |key: &str, field: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, item)| {
                item.get(field)
                    .and_then(|n| n.as_str())
                    .filter(|n| !n.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{key} {}", i + 1))
            })
            .collect()
    };

    Json(StudySummary {
        name: value.get("name").and_then(|n| n.as_str()).unwrap_or(&slug).to_string(),
        editable: value.get("_embarch_ui_rows").is_some(),
        requires: RequirementsOut {
            dev_bench_version: version_field(&value, "dev_bench_version"),
            firmware_version: version_field(&value, "firmware_version"),
            build: requires_object(&value, "build"),
            outpost: requires_object(&value, "outpost"),
        },
        // Absent means the field predates `dev_bench_log_level`, and such a
        // study ran at the crate's default — which is what is reported,
        // rather than an empty cell that reads as "nothing".
        dev_bench_log_level: value
            .get("dev_bench_log_level")
            .and_then(|l| l.as_str())
            .unwrap_or("Warn")
            .to_string(),
        steps: list("steps", "name"),
        taps: list("streams", "name"),
        protocols: list("protocols", "name"),
        record_checks: value
            .get("record_checks")
            .and_then(|c| c.as_array())
            .map(|c| c.len())
            .unwrap_or(0),
        slug,
    })
    .into_response()
}

/// Reads and parses one saved study file, or the response that says why not.
///
/// The `Err` is boxed: an `axum::Response` is a 128-byte value, and
/// `clippy::result_large_err` is right that a `Result` shaped that way costs
/// every caller a move it does not need. Boxing is one line where an
/// `#[allow]` would be one line of silence.
fn read_saved_study(
    project: &StudyDesignerConfig,
    slug: &str,
) -> Result<serde_json::Value, Box<axum::response::Response>> {
    let path = studies_dir(project).join(format!("{slug}.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Box::new(
                (StatusCode::NOT_FOUND, format!("no saved study '{slug}'")).into_response(),
            ))
        }
        Err(e) => {
            return Err(Box::new(
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            ))
        }
    };
    serde_json::from_str(&text).map_err(|e| {
        Box::new(
            (StatusCode::BAD_REQUEST, format!("{} isn't valid JSON: {e}", path.display()))
                .into_response(),
        )
    })
}

/// Runs a saved study **as it is on disk**, with no round trip through the
/// table.
///
/// This is the only path a run-only file has: `api_studies_load` answers
/// `409` for a study with no `_embarch_ui_rows`, and that refusal is right —
/// there are no rows to load. What was missing was a way to run one anyway,
/// which is the whole reason such files exist.
///
/// **Every seal is recomputed before submitting** (decision 26), all three,
/// because a hand-written file's seals are whatever its author typed. Then
/// the crate's own pre-flight runs locally — `validate_taps`,
/// `validate_protocol` per carried protocol, and the two `RunProtocol` index
/// checks — so an authoring mistake in that file is a sentence here rather
/// than a `400` from a round trip.
pub async fn api_study_run(
    State(state): State<crate::AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
    body: Option<Json<StudyRunRequest>>,
) -> axum::response::Response {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    run_saved_study(&state, &slug, req.allow_version_mismatch).await
}

/// [`api_study_run`]'s whole body, reachable by name.
///
/// Two routes post a saved study now — the Study Designer's own Run button
/// and the Live Study tab's — and **one of them had to not be a second
/// implementation.** Everything a run decides lives here: the seals, the
/// crate's own pre-flight, the submit, and the hand-off to the live session.
pub async fn run_saved_study(
    state: &crate::AppState,
    slug: &str,
    allow_version_mismatch: bool,
) -> axum::response::Response {
    let sd = state.study_designer.clone();
    let Some(project) = sd.project() else { return not_configured() };
    let slug = match study_slug(slug) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let value = match read_saved_study(&project, &slug) {
        Ok(v) => v,
        Err(response) => return *response,
    };
    // **A deserialize failure is a fact about that file**, stated as one —
    // not a `502` about a round trip that never happened, and not a bare
    // serde message with no path. Capacity diagnosis (which `embarch-api`'s
    // `capacity::explain` does properly) is private to that binary, so this
    // reports serde's own line and column and points at the CLI that can say
    // more.
    let mut study: Study = match serde_json::from_value(value) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "'{slug}.json' is not a Study this build can run: {e}. \
                     `embarch-api run-study --study-file` reports capacity limits in more \
                     detail."
                ),
            )
                .into_response()
        }
    };

    if let Err(e) = seal_crc(&mut study) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Err(e) = preflight_saved(&study) {
        return (StatusCode::BAD_REQUEST, format!("'{slug}.json': {e}")).into_response();
    }

    // The slug is passed on so a build that ran under a waved-through
    // version mismatch can write what it flashed back into this file — see
    // `submit_run`, which is also where the rule that it usually does not
    // lives.
    submit_run(state, study, allow_version_mismatch, Some(slug)).await
}

#[derive(Debug, Default, Deserialize)]
pub struct StudyRunRequest {
    #[serde(default)]
    allow_version_mismatch: bool,
}

/// The crate's own pre-flight over a study nobody here built.
///
/// An authored study got these checks on the way through `build_study` and
/// `build_taps`; a file read off disk got none of them. Run here so a
/// mistake in a hand-written file is a sentence in this tab.
fn preflight_saved(study: &Study) -> Result<(), String> {
    embarch_study_designer::validate_taps(
        &study.streams,
        study.steps.len() as u32,
        study.decoders.len(),
    )
    .map_err(|e| format!("{e:?}"))?;

    for protocol in study.protocols.iter() {
        embarch_study_designer::validate_protocol(protocol)
            .map_err(|e| format!("protocol '{}': {e}", protocol.name))?;
    }

    // The two index checks `validate_protocol` cannot make, because it never
    // sees an `Action` — see `Action::RunProtocol`'s own comment. Both would
    // otherwise reach a hand-written C array subscript on the bench.
    for (i, step) in study.steps.iter().enumerate() {
        let Action::RunProtocol { protocol, entry_state } = step.action else { continue };
        let Some(def) = study.protocols.get(protocol as usize) else {
            return Err(format!(
                "step {} ('{}') runs protocol {protocol}, but this study carries {}",
                i + 1,
                step.name,
                study.protocols.len()
            ));
        };
        let Some(state) = def.states.get(entry_state as usize) else {
            return Err(format!(
                "step {} ('{}') enters protocol '{}' at state {entry_state}, which declares {}",
                i + 1,
                step.name,
                def.name,
                def.states.len()
            ));
        };
        if matches!(state.kind, embarch_study_designer::StateKind::Terminal(_)) {
            return Err(format!(
                "step {} ('{}') enters protocol '{}' at '{}', a terminal state — the run \
                 would reach its outcome immediately and capture nothing",
                i + 1,
                step.name,
                def.name,
                state.name
            ));
        }
    }
    Ok(())
}

// ---- saved study library (`embarch-study-designer` decision 38) ------------
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

/// `base`, or the first `base-N` that no file is using.
///
/// Counts from 2 because the first one is `base` itself, and stops at a
/// bound rather than looping forever: a directory that somehow holds every
/// suffix is a state worth landing *somewhere* in, and the caller's `exists`
/// check on the returned path is what would catch it.
fn first_free_slug(dir: &std::path::Path, base: &str) -> (String, std::path::PathBuf) {
    let mut slug = base.to_string();
    let mut path = dir.join(format!("{slug}.json"));
    let mut n = 2;
    while path.exists() && n < 1000 {
        slug = format!("{base}-{n}");
        path = dir.join(format!("{slug}.json"));
        n += 1;
    }
    (slug, path)
}

/// `<firmware repo>/embarch/studies` (`embarch-study-designer` decision
/// 38) — taken from the *open project* rather than from config, so
/// switching projects moves the studies list with it (decision 14).
fn studies_dir(project: &StudyDesignerConfig) -> std::path::PathBuf {
    project.firmware_repo_path.join("embarch").join("studies")
}

// ---- the reference scan -------------------------------------------------
//
// Three things in a firmware repo can be deleted or renamed out from under a
// saved study: a registered action, a payload layout, and an `.eap` protocol.
// A destructive edit that breaks a saved study is **refused, naming the
// studies** rather than performed and reported — the owner's call, and the
// only one that leaves an engineer able to decide what to do.
//
// This scan lives beside `studies_dir` because that is the directory it
// reads and `_embarch_ui_rows` is the key it reads; it is deliberately not
// in `embarch-study-designer`, which knows nothing about this tab's sidecar.
//
// It reads each file as a `serde_json::Value`, exactly as `api_studies_list`
// already does, rather than deserializing a `Study`: a hand-written file, a
// file from an older schema, and a file this tab wrote all have to be
// scanned, and the strictest of the three would refuse the other two.

/// What a scan names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefKind {
    /// A `study-actions.toml` entry, by name.
    Action,
    /// A `study-structs.toml` layout, by name.
    Layout,
    /// An `.eap` protocol, by name.
    Protocol,
}

/// One saved study that references the thing being deleted.
#[derive(Debug, Clone, Serialize)]
pub struct StudyReference {
    slug: String,
    name: String,
    /// Which of its steps or taps name it, so an engineer knows where to
    /// look rather than only that a study somewhere does.
    steps: Vec<String>,
}

/// The whole answer to "is anything using this".
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReferenceScan {
    referenced_by: Vec<StudyReference>,
    /// Files in the studies directory that could not be read or parsed.
    ///
    /// **Never folded into "no references".** A directory this cannot fully
    /// read is not permission to delete: a file it could not parse might be
    /// the one study using the thing about to disappear. A non-empty list
    /// refuses the delete exactly as a reference does.
    unscannable: Vec<String>,
}

impl ReferenceScan {
    /// Whether a destructive edit should be refused.
    fn blocks(&self) -> bool {
        !self.referenced_by.is_empty() || !self.unscannable.is_empty()
    }
}

/// Every saved study that names `target`.
///
/// A missing studies directory is an empty scan: nothing can reference
/// anything, which is the ordinary state of a repo that has saved no study.
fn scan_references(
    project: &StudyDesignerConfig,
    kind: RefKind,
    target: &str,
) -> ReferenceScan {
    let mut scan = ReferenceScan::default();
    let dir = studies_dir(project);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return scan,
        Err(e) => {
            scan.unscannable.push(format!("{}: {e}", dir.display()));
            return scan;
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            scan.unscannable.push(format!("{}: an entry could not be read", dir.display()));
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let slug = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            scan.unscannable.push(format!("{slug}.json could not be read"));
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            scan.unscannable.push(format!("{slug}.json is not valid JSON"));
            continue;
        };

        let steps = references_in(&value, kind, target);
        if !steps.is_empty() {
            scan.referenced_by.push(StudyReference {
                name: value.get("name").and_then(|n| n.as_str()).unwrap_or(&slug).to_string(),
                slug,
                steps,
            });
        }
    }
    scan.referenced_by.sort_by(|a, b| a.slug.cmp(&b.slug));
    scan
}

/// Which parts of one saved study name `target`.
///
/// A layout is looked for in **both** places it can appear: `decoders[].name`
/// — the resolved copy that actually runs — and `_embarch_ui_taps[].decoder`,
/// the authored name. Either alone would miss a real reference: a hand-written
/// study has no sidecar, and a study whose tap names a layout that failed to
/// resolve has no `decoders` entry.
///
/// A protocol likewise: `protocols[].name` is what runs,
/// `_embarch_ui_rows[].action.protocol` is what was authored.
fn references_in(value: &serde_json::Value, kind: RefKind, target: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut note = |label: String| {
        if !out.contains(&label) {
            out.push(label);
        }
    };

    let rows = value.get("_embarch_ui_rows").and_then(|r| r.as_array());
    let step_name = |row: &serde_json::Value, index: usize| -> String {
        row.get("name")
            .and_then(|n| n.as_str())
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("step {}", index + 1))
    };

    match kind {
        RefKind::Action => {
            for (i, row) in rows.into_iter().flatten().enumerate() {
                let action = row.get("action");
                let names_it = action
                    .and_then(|a| a.get("name"))
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n == target);
                if names_it {
                    note(step_name(row, i));
                }
            }
        }
        RefKind::Protocol => {
            for (i, row) in rows.into_iter().flatten().enumerate() {
                let names_it = row
                    .get("action")
                    .and_then(|a| a.get("protocol"))
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n == target);
                if names_it {
                    note(step_name(row, i));
                }
            }
            let carried = value
                .get("protocols")
                .and_then(|p| p.as_array())
                .into_iter()
                .flatten()
                .any(|p| p.get("name").and_then(|n| n.as_str()) == Some(target));
            if carried {
                note("the study carries this protocol".to_string());
            }
        }
        RefKind::Layout => {
            let carried = value
                .get("decoders")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
                .any(|d| d.get("name").and_then(|n| n.as_str()) == Some(target));
            if carried {
                note("the study carries this layout".to_string());
            }
            for (i, tap) in value
                .get("_embarch_ui_taps")
                .and_then(|t| t.as_array())
                .into_iter()
                .flatten()
                .enumerate()
            {
                if tap.get("decoder").and_then(|d| d.as_str()) == Some(target) {
                    note(
                        tap.get("name")
                            .and_then(|n| n.as_str())
                            .filter(|n| !n.trim().is_empty())
                            .map(|n| format!("tap '{n}'"))
                            .unwrap_or_else(|| format!("tap {}", i + 1)),
                    );
                }
            }
        }
    }
    out
}

/// The one structured error body in this file.
///
/// Every other error here is plain text, and the browser renders an
/// unparseable refusal verbatim — which is what keeps a `500` from a layer
/// that never heard of this shape readable. This one is JSON because its
/// payload is a *list* the dialog renders as a table, and a table
/// reconstructed by splitting a sentence is a parser.
fn refusal(what: &str, name: &str, scan: &ReferenceScan) -> axum::response::Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": format!(
                "{what} '{name}' is still used by {} saved {}",
                scan.referenced_by.len(),
                if scan.referenced_by.len() == 1 { "study" } else { "studies" }
            ),
            "referenced_by": scan.referenced_by,
            "unscannable": scan.unscannable,
        })),
    )
        .into_response()
}

// ---- `.eap` protocol files (`embarch-study-designer` decisions 58-62) ----
//
// One directory at `<firmware-repo>/embarch/protocols/`, one `.eap` file per
// handshake, edited as **text with live parse errors** rather than through a
// structured form. A form would be a second copy of a server-side grammar in
// JavaScript, which is the largest possible form of the defect `suite/017`
// just closed; the study view shows names only.
//
// Every route here goes through `eap_repo`, which parses and resolves before
// it writes, so this tab never leaves text on disk that this crate would
// refuse to read back.

/// One `.eap` file, as `GET /protocols` lists it.
#[derive(Debug, Serialize)]
struct ProtocolFileSummary {
    stem: String,
    /// The file's whole text — what the editor opens.
    ///
    /// **Served with the listing, not fetched per file.** `eap_repo::scan`
    /// has already read every one of them, so a second round trip would
    /// re-read what is in hand; and a per-select fetch is a request that can
    /// fail while somebody is switching files with unsaved work, which is
    /// the one moment this dialog must not be uncertain. These are short
    /// hand-written text files.
    text: String,
    /// Every block that resolved, with its states — the same shape the
    /// actions response serves, so the editor and the row pickers read one
    /// vocabulary.
    protocols: Vec<ProtocolSummary>,
    /// This file's errors: its one parse error, or one per block that failed
    /// to resolve.
    ///
    /// **At most one parse error**, because the parser stops at the first
    /// thing it cannot read. The editor does not promise a multi-error list
    /// the parser cannot produce.
    errors: Vec<EapErrorOut>,
}

/// One error, with the line the editor bands.
#[derive(Debug, Clone, Serialize)]
pub struct EapErrorOut {
    /// `EapError`'s own `line {n}: …` sentence, rendered by the crate.
    message: String,
    /// 1-based source line, or `0` for an error with no line — a file that
    /// could not be read at all. **A line-0 error gets no band.**
    line: u32,
}

impl EapErrorOut {
    fn from(error: &embarch_study_designer::eap_repo::FileError) -> EapErrorOut {
        let line = match error {
            embarch_study_designer::eap_repo::RepoError::Eap(e) => e.line,
            _ => 0,
        };
        EapErrorOut { message: error.to_string(), line }
    }
}

#[derive(Debug, Serialize)]
struct ProtocolsResponse {
    files: Vec<ProtocolFileSummary>,
    /// Protocol names declared by more than one file, each with the stems
    /// declaring it.
    ///
    /// Rendered repo-wide, above the files: this is the one situation
    /// `eap_repo::defs` refuses outright, and seeing it here is how an
    /// author finds out before a build fails rather than when one does.
    duplicate_names: Vec<DuplicateName>,
}

#[derive(Debug, Serialize)]
struct DuplicateName {
    name: String,
    files: Vec<String>,
}

fn protocols_response(repo: &RepoProtocols) -> ProtocolsResponse {
    ProtocolsResponse {
        files: repo
            .files
            .iter()
            .map(|file| ProtocolFileSummary {
                stem: file.stem.clone(),
                text: file.text.clone(),
                protocols: file
                    .resolved()
                    .map(|(name, resolved)| ProtocolSummary {
                        name: name.to_string(),
                        file: file.stem.clone(),
                        resolved: true,
                        states: state_summaries(resolved),
                    })
                    .collect(),
                errors: file.errors().iter().map(EapErrorOut::from).collect(),
            })
            .collect(),
        duplicate_names: repo
            .duplicate_names()
            .into_iter()
            .map(|(name, files)| DuplicateName { name, files })
            .collect(),
    }
}

pub async fn api_protocols(State(state): State<crate::AppState>) -> axum::response::Response {
    let sd = state.study_designer;
    if sd.project().is_none() {
        return not_configured();
    }
    match sd.protocols() {
        Ok(repo) => Json(protocols_response(&repo)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[derive(Debug, Serialize)]
struct ProtocolFileOut {
    stem: String,
    /// The file's whole text — what the editor opens.
    text: String,
    protocols: Vec<ProtocolSummary>,
    errors: Vec<EapErrorOut>,
}

pub async fn api_protocol_read(
    State(state): State<crate::AppState>,
    axum::extract::Path(stem): axum::extract::Path<String>,
) -> axum::response::Response {
    let sd = state.study_designer;
    if sd.project().is_none() {
        return not_configured();
    }
    if let Err(e) = embarch_study_designer::eap_repo::validate_stem(&stem) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let repo = match sd.protocols() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let Some(file) = repo.file(&stem) else {
        return (StatusCode::NOT_FOUND, format!("no protocol file '{stem}.eap'")).into_response();
    };
    Json(ProtocolFileOut {
        stem: file.stem.clone(),
        text: file.text.clone(),
        protocols: file
            .resolved()
            .map(|(name, resolved)| ProtocolSummary {
                name: name.to_string(),
                file: file.stem.clone(),
                resolved: true,
                states: state_summaries(resolved),
            })
            .collect(),
        errors: file.errors().iter().map(EapErrorOut::from).collect(),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ProtocolTextRequest {
    text: String,
}

/// Writes one `.eap` file, refusing text that does not parse or resolve.
///
/// The `400` carries `EapError`'s own `line {n}: …` sentence unchanged — the
/// editor bands that line from it, and a reworded message would mean the
/// editor and the error disagree about where the problem is.
///
/// **Nothing is written on a refusal**, so a failed save cannot cost an
/// engineer the working version they were editing away from; `eap_repo::save`
/// is where that ordering lives.
pub async fn api_protocol_write(
    State(state): State<crate::AppState>,
    axum::extract::Path(stem): axum::extract::Path<String>,
    Json(req): Json<ProtocolTextRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    // Validated here and again inside `eap_repo::save` — traversal is
    // refused twice, deliberately. The check beside the `join` is the one
    // that protects the filesystem; this one gives a better message.
    if let Err(e) = embarch_study_designer::eap_repo::validate_stem(&stem) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    match embarch_study_designer::eap_repo::save(&project.firmware_repo_path, &stem, &req.text) {
        Ok(()) => match sd.protocols() {
            Ok(repo) => Json(protocols_response(&repo)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
        Err(embarch_study_designer::eap_repo::RepoError::Io(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// Deletes one `.eap` file, refusing while a saved study still names one of
/// its protocols.
pub async fn api_protocol_delete(
    State(state): State<crate::AppState>,
    axum::extract::Path(stem): axum::extract::Path<String>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };
    if let Err(e) = embarch_study_designer::eap_repo::validate_stem(&stem) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let repo = match sd.protocols() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let Some(file) = repo.file(&stem) else {
        return (StatusCode::NOT_FOUND, format!("no protocol file '{stem}.eap'")).into_response();
    };

    // Every protocol this file declares, checked one at a time, so the
    // refusal names the protocol a study actually uses rather than the file.
    //
    // A file that did not parse declares no resolvable protocol and is
    // therefore deletable by this check alone — which is right: nothing can
    // be running it. `scan_references`'s own unscannable list is what keeps
    // that from being a hole, since a study naming it by an authored name
    // still matches.
    let mut merged = ReferenceScan::default();
    for (name, _) in file.resolved() {
        let scan = scan_references(&project, RefKind::Protocol, name);
        merged.referenced_by.extend(scan.referenced_by);
        merged.unscannable.extend(scan.unscannable);
    }
    // A file that did not parse still has to be checked against its own
    // stem, because an author who named a protocol after its file is the
    // ordinary case and the parse failure is exactly why it stopped
    // resolving.
    if file.parsed.is_err() {
        let scan = scan_references(&project, RefKind::Protocol, &stem);
        merged.referenced_by.extend(scan.referenced_by);
        merged.unscannable.extend(scan.unscannable);
    }
    merged.unscannable.sort();
    merged.unscannable.dedup();
    if merged.blocks() {
        return refusal("protocol file", &stem, &merged);
    }

    match embarch_study_designer::eap_repo::delete(&project.firmware_repo_path, &stem) {
        Ok(()) => match sd.protocols() {
            Ok(repo) => Json(protocols_response(&repo)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Serialize)]
struct ProtocolCheckResponse {
    ok: bool,
    protocols: Vec<ProtocolSummary>,
    errors: Vec<EapErrorOut>,
}

/// Parses `.eap` text without writing it — the editor's Check button.
///
/// **`200` even when `ok` is false.** Reporting what is wrong is this
/// route's entire purpose, so a failed parse is a successful check; the same
/// shape `api_version_check` already uses for a mismatch it is reporting
/// rather than suffering. A `400` here would make the browser's error path
/// and its success path both have to render errors.
pub async fn api_protocol_check(
    State(state): State<crate::AppState>,
    Json(req): Json<ProtocolTextRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    if sd.project().is_none() {
        return not_configured();
    }
    let (protocols, errors) = match embarch_study_designer::eap_parse::parse(&req.text) {
        Err(e) => (Vec::new(), vec![EapErrorOut { message: e.to_string(), line: e.line }]),
        Ok(file) => {
            let mut protocols = Vec::new();
            let mut errors = Vec::new();
            for block in &file.protocols {
                match embarch_study_designer::eap_parse::resolve(block) {
                    Ok(resolved) => protocols.push(ProtocolSummary {
                        name: block.name.clone(),
                        file: String::new(),
                        resolved: true,
                        states: state_summaries(&resolved),
                    }),
                    Err(e) => errors.push(EapErrorOut { message: e.to_string(), line: e.line }),
                }
            }
            (protocols, errors)
        }
    };
    Json(ProtocolCheckResponse { ok: errors.is_empty(), protocols, errors }).into_response()
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
    /// How loud dev-bench's firmware should be for this run
    /// (`embarch-dev-bench` decision 39). Absent leaves the crate's own
    /// default (`Warn`) in place — see `build_authored`, which is the one
    /// place that decision is read.
    ///
    /// A property of the **saved study**, not of the run: decision 51
    /// rejected making it a property of the log tap, so it is saved and
    /// loaded with the study exactly as `requires` is, and it is not a run
    /// dialog field the way `reflash` and `allow_version_mismatch` are.
    #[serde(default)]
    dev_bench_log_level: Option<DevBenchLogLevel>,
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
    /// Read out of the saved `Study` itself, like `requires` and for the same
    /// reason: the level is a real field of the thing that runs.
    ///
    /// `None` for a file authored before the field existed. Loading that as
    /// `None` rather than as the default's spelling is what keeps "not
    /// stated" distinguishable after a round trip — a study reloaded and
    /// re-saved must not gain an explicit level it never had.
    dev_bench_log_level: Option<String>,
}

#[derive(Debug, Serialize)]
struct RequirementsOut {
    dev_bench_version: String,
    firmware_version: String,
    /// The build spec and the outpost mode, **passed back verbatim** rather
    /// than reshaped into a typed mirror.
    ///
    /// Both are authored by the Build card and read back by it, and the one
    /// authority on their shape is `embarch-study-designer`. A typed copy
    /// here would be a third spelling of the same thing — after
    /// `BuildSpecInput` and `BuildSpec` — whose only job would be to agree
    /// with the other two, which is the sort of agreement that lapses
    /// quietly. `null` is a study that declares neither, which is most of
    /// them.
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outpost: Option<serde_json::Value>,
}

/// One `requires` sub-object as it was saved, or `None`.
///
/// `null` and absent are both `None` on purpose: a study saved before these
/// fields existed has neither key, and one saved with the toggle off has
/// them as `null`, and they mean the same thing to a reader.
fn requires_object(value: &serde_json::Value, field: &str) -> Option<serde_json::Value> {
    value
        .get("requires")
        .and_then(|r| r.get(field))
        .filter(|v| !v.is_null())
        .cloned()
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
    // A repo whose `.eap` files declare one protocol name twice is refused
    // here rather than resolved by directory order — see `sd.protocol_defs`.
    // `400` and not `500`: the repo's own files are what is wrong, and the
    // person who can fix them is the one looking at this tab.
    let protocols = match sd.protocol_defs() {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let study = match build_authored(
        &req.name,
        &req.rows,
        &req.requires,
        &req.taps,
        &registry,
        &structs,
        &protocols,
        req.dev_bench_log_level,
    ) {
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
        build: requires_object(&value, "build"),
        outpost: requires_object(&value, "outpost"),
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
        dev_bench_log_level: value
            .get("dev_bench_log_level")
            .and_then(|l| l.as_str())
            .map(str::to_string),
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
        .enumerate()
        .filter_map(|(index, tap)| {
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
            // The record magic comes back the same way, off
            // `Study.record_checks` keyed by `stream_id` — which is the tap's
            // own index, assigned by `build_taps` and enforced by
            // `validate_taps`, so this is a lookup and not a guess either. A
            // tap with no check gets an empty magic: no check, which is what
            // such a tap had.
            let record_magic = value
                .get("record_checks")
                .and_then(|c| c.as_array())
                .and_then(|checks| {
                    let check = checks.iter().find(|c| {
                        c.get("stream_id").and_then(|id| id.as_u64()) == Some(index as u64)
                    })?;
                    let magic = check.get("framing")?.get("MagicPrefixedCrc32Le")?.get("magic")?;
                    Some(
                        magic
                            .as_array()?
                            .iter()
                            .filter_map(|b| u8::try_from(b.as_u64()?).ok())
                            .collect::<Vec<u8>>(),
                    )
                })
                .unwrap_or_default();
            Some(TapInput::GattNotify {
                name,
                service_uuid: uuid_field(gatt, "service_uuid")?,
                characteristic_uuid: uuid_field(gatt, "characteristic_uuid")?,
                decoder,
                record_magic,
            })
        })
        .collect()
}

/// A `Uuid` in a saved `Study` is a JSON array of 16 bytes (its `Serialize`
/// is the raw form, `embarch-study-designer/interfaces/types.md`); the
/// table works in the hyphenated text an engineer reads. This is the one
/// place that conversion happens on the load path.
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
/// `embarch-study-designer` decision 40 exists to close (decision 11).
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

/// Serves a finished study's GATT transcript (`embarch-study-designer`
/// decision 36) straight through from Core, so the browser downloads it
/// over embarch-ui's own origin and never needs Core's bearer token — the
/// same reason `/api/enroll` exists rather than the browser calling Core.
///
/// **Two calls, not one.** This used to be a single `get_study_gatt_data`
/// over Core's `/gatt-data` alias; that alias is retired, and its one virtue
/// — not needing the tap's name — is exactly what made it unable to report a
/// truncated capture. So the tap is looked up by its declared
/// `GattTranscript` encoding in the study's own stream index, and then
/// fetched by name through the generic route.
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

    let index = match sd.0.core.study_streams(&study_id).await {
        Ok(Some(index)) => index,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("study '{study_id}' recorded no streams"))
                .into_response()
        }
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    let Some(name) = index
        .streams
        .iter()
        .find(|e| matches!(e.encoding, StreamEncoding::GattTranscript))
        .map(|e| e.name.clone())
    else {
        return (
            StatusCode::NOT_FOUND,
            format!("study '{study_id}' declared no GATT transcript tap"),
        )
            .into_response();
    };

    match sd.0.core.get_study_stream(&study_id, &name, false).await {
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

    /// **A text guard, not a behavioural test, and it is the only thing
    /// covering this at all.** `assets/app.js` is served as bytes and never
    /// evaluated by `cargo test` — there is no JS engine on this bench, which
    /// is why `trace.rs`'s browser harness dumps JSON for a manual headless
    /// Firefox run instead. So the badge's arithmetic can only be pinned by
    /// its source text.
    ///
    /// What it pins: the counter still goes through one named helper, still
    /// adds *two* to Core's last-finished index, and is still clamped — so a
    /// later reader who "corrects" it back to the count convention trips a
    /// test rather than shipping a badge that is one step short again
    /// (`embarch-ui/decisions/study-designer.md` decision 20).
    #[test]
    fn run_badge_counter_names_the_step_now_running() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert!(
            APP_JS.contains("function sdRunningStepLabel(currentStep, totalSteps)"),
            "the badge's arithmetic lives in one named helper"
        );
        assert!(
            APP_JS.contains("var step = currentStep == null ? 1 : currentStep + 2;"),
            "the step now running is Core's last-finished index + 2, or 1 before any has finished"
        );
        assert!(
            APP_JS.contains("if (step > totalSteps) step = totalSteps;"),
            "clamped for the window between the last step landing and Core reporting `completed`"
        );
        assert!(
            !APP_JS.contains("(state.current_step + 1)"),
            "the count-convention `+ 1` is exactly the defect decision 20 closes"
        );
    }

    /// The served cap is the enforced cap, for every cap and vocabulary on
    /// this response.
    ///
    /// **The struct literal is the guard.** A field added to
    /// `ActionsResponse` without a line here does not compile, which is what
    /// makes this catch a *new* served fact that quietly gets a wrong value,
    /// not only a changed one. `app.js` has no fallback for any of these —
    /// see the negative text guards below, which pin that it holds no copy.
    #[test]
    fn every_served_limit_and_vocabulary_matches_its_constant() {
        use embarch_study_designer::limits;

        let response = ActionsResponse {
            actions: Vec::new(),
            live_gatt_available: false,
            static_gatt_available: false,
            subscribable: Vec::new(),
            struct_layouts: Vec::new(),
            characteristic_names: BTreeMap::new(),
            service_names: BTreeMap::new(),
            protocols: Vec::new(),
            unparsed_files: Vec::new(),
            max_monitor_targets: limits::MAX_MONITOR_TARGETS,
            max_stream_name_len: MAX_STREAM_NAME_LEN,
            max_protocols_per_study: limits::MAX_PROTOCOLS_PER_STUDY,
            max_record_magic_len: MAX_RECORD_MAGIC_LEN,
            max_struct_fields: limits::MAX_STRUCT_FIELDS,
            scalar_types: embarch_study_designer::ScalarType::ALL
                .iter()
                .map(|t| t.as_str())
                .collect(),
            dev_bench_log_levels: DevBenchLogLevel::ALL
                .iter()
                .map(|l| LogLevelOption {
                    value: serde_json::to_value(l).unwrap().as_str().unwrap().to_string(),
                    label: l.label(),
                    note: log_level_note(*l),
                    default: *l == DevBenchLogLevel::default(),
                })
                .collect(),
            dev_bench_limits: DevBenchLimits {
                max_steps_per_study: limits::DEV_BENCH_MAX_STEPS_PER_STUDY,
                max_event_arms_per_state: limits::DEV_BENCH_MAX_EVENT_ARMS_PER_STATE,
                max_protocols_wire_len: limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN,
            },
        };
        let json = serde_json::to_value(&response).unwrap();

        assert_eq!(json["max_stream_name_len"], MAX_STREAM_NAME_LEN);
        assert_eq!(json["max_monitor_targets"], limits::MAX_MONITOR_TARGETS);
        assert_eq!(json["max_protocols_per_study"], limits::MAX_PROTOCOLS_PER_STUDY);
        assert_eq!(json["max_record_magic_len"], MAX_RECORD_MAGIC_LEN);
        assert_eq!(json["max_struct_fields"], limits::MAX_STRUCT_FIELDS);
        assert_eq!(json["scalar_types"].as_array().unwrap().len(), 18);
        assert_eq!(json["scalar_types"][0], "u8");
        assert_eq!(json["dev_bench_log_levels"].as_array().unwrap().len(), 5);
        // The JSON spelling a saved study carries, served as-is — and the
        // default flagged here rather than left for a browser to assert.
        assert_eq!(json["dev_bench_log_levels"][2]["value"], "Warn");
        assert_eq!(json["dev_bench_log_levels"][2]["default"], true);
        assert_eq!(
            json["dev_bench_log_levels"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|l| l["default"] == true)
                .count(),
            1,
            "exactly one level is the default"
        );
        // `Off` warns about the dump it loses; the two loud ones warn about
        // the clamp. The two quiet middle levels have nothing to say.
        assert!(json["dev_bench_log_levels"][0]["note"]
            .as_str()
            .unwrap()
            .contains("fatal-error dump"));
        assert_eq!(json["dev_bench_log_levels"][1]["note"], "");
        assert_eq!(json["dev_bench_log_levels"][2]["note"], "");
        assert!(json["dev_bench_log_levels"][4]["note"].as_str().unwrap().contains("clamps"));
        assert_eq!(
            json["dev_bench_limits"]["max_steps_per_study"],
            limits::DEV_BENCH_MAX_STEPS_PER_STUDY
        );
        assert_eq!(
            json["dev_bench_limits"]["max_event_arms_per_state"],
            limits::DEV_BENCH_MAX_EVENT_ARMS_PER_STATE
        );
        assert_eq!(
            json["dev_bench_limits"]["max_protocols_wire_len"],
            limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN
        );
    }

    /// The three record-cell invariants, as text guards — the only thing
    /// that can cover them in `cargo test`, since `app.js` is served as
    /// bytes and never evaluated here.
    ///
    /// Each has a wrong version that looks right, which is why they are
    /// pinned rather than left to reading: a `records: null` rendered as
    /// clean, an empty capture rendered as verified, and a literal `32`
    /// standing in for `MAX_BAD_RECORDS_REPORTED`.
    #[test]
    fn the_records_cell_never_reads_clean_when_it_cannot_know() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert!(APP_JS.contains("function recordsCell(report)"), "one named helper");
        assert!(
            APP_JS.contains("not checked — this tap declared no record framing"),
            "`records: null` reads as not checked, never as clean"
        );
        assert!(
            APP_JS.contains("nothing to check — no record found in this capture"),
            "an empty capture reads as nothing to check, never as verified"
        );
        assert!(
            !APP_JS.contains("report.all_verified"),
            "RecordReport::all_verified() returns true for an empty capture, so the browser \
             deliberately does not call it — the comment naming it is the reason why"
        );
        assert!(
            APP_JS.contains("var capped = offsets.length < bad;"),
            "the 32-offset cap is inferred by comparing offsets with failures, never written"
        );
        // Scoped to the helper's own body: `32` appears elsewhere in this
        // file as a UUID hex length, and a whole-file substring check would
        // be a guard that fails for an unrelated reason.
        let body = {
            let start = APP_JS.find("function recordsCell(report)").unwrap();
            let rest = &APP_JS[start..];
            &rest[..rest.find("\n  }\n").unwrap()]
        };
        assert!(
            !body.contains(&embarch_study_designer::limits::MAX_BAD_RECORDS_REPORTED.to_string()),
            "a literal MAX_BAD_RECORDS_REPORTED in recordsCell is a copy that goes stale"
        );
    }

    /// A protocol outcome goes through the same `outcomeBadge` every other
    /// outcome does — one decoder, so a protocol that failed cannot read as
    /// one that did not — and `final_state` is rendered with no claim about
    /// whether it was terminal, which is a lookup this file cannot make.
    #[test]
    fn a_protocol_outcome_reuses_the_one_outcome_decoder() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert!(
            APP_JS.contains("var badge = outcomeBadge(step.protocol.outcome, null);"),
            "through the existing badge, not a second decoder"
        );
        assert!(
            APP_JS.contains("escapeHtml(step.protocol.final_state || \"—\")"),
            "final_state is rendered verbatim"
        );
        for claim in ["finished", "reached its outcome", "terminal state"] {
            assert!(
                !APP_JS.contains(&format!("ended in {claim}")),
                "the browser must claim nothing about whether final_state was terminal"
            );
        }
    }

    /// The record-framing column is emitted **once**, from `renderSdTaps`,
    /// so both tap kinds go through one insertion.
    ///
    /// A column added inside each kind's own cell builder is a column that
    /// gets added to one and forgotten on the other, which is a row with the
    /// wrong number of cells and a table that silently shears.
    #[test]
    fn the_record_framing_column_is_emitted_once_for_both_tap_kinds() {
        const APP_JS: &str = include_str!("../assets/app.js");
        // Two occurrences: the declaration and exactly one call site.
        assert_eq!(
            APP_JS.matches("sdRecordFramingCell(tap, i)").count(),
            2,
            "one insertion, in renderSdTaps, not one per tap kind"
        );
        assert_eq!(APP_JS.matches("function sdRecordFramingCell").count(), 1);
        assert!(
            APP_JS.contains("not applicable — an outpost trace is a raw"),
            "an outpost row says why it has no control rather than offering one that does nothing"
        );
        assert!(
            APP_JS.contains("parseBytes(text, tap.magicMode === \"text\" ? \"text\" : \"hex\")"),
            "the magic goes through the same parseBytes the registration form uses, unchanged"
        );
        assert!(
            APP_JS.contains("and a magic is never shortened to fit"),
            "an over-long magic is named, never trimmed"
        );
    }

    /// A run-only study never reaches the load route, and its Run goes
    /// through the **existing** version-check dialog.
    ///
    /// Two properties worth pinning: `sdOpenStored` must not touch `sdRows`
    /// or `sdTaps` (a read-only preview that edited the table would be the
    /// silent overwrite the whole panel exists to avoid saying), and there
    /// must be exactly one version-check dialog, because the question it
    /// asks is the same question either way.
    #[test]
    fn a_run_only_study_previews_without_touching_the_table() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert!(
            APP_JS.contains("if (slug && known && known.editable === false) return sdOpenStored(slug);"),
            "a run-only file is recognised before the load route is called, not by its 409"
        );
        let body = {
            let start = APP_JS.find("async function sdOpenStored(slug)").unwrap();
            let rest = &APP_JS[start..];
            &rest[..rest.find("\n  }\n").unwrap()]
        };
        assert!(!body.contains("sdRows"), "the preview must not touch the step table");
        assert!(!body.contains("sdTaps"), "the preview must not touch the taps");
        assert!(
            body.contains("The step table above is a different study"),
            "the panel says out loud that the table above is a different study"
        );
        assert_eq!(
            APP_JS.matches("async function sdOpenRunCheck()").count(),
            1,
            "one version-check dialog, reached by both kinds of run"
        );
        assert!(
            APP_JS.contains("function sdDispatchRun(allowMismatch)"),
            "one Run button in that dialog, two destinations"
        );
    }

    /// The layout editor offers **only** served scalar types, and a refusal
    /// leaves the form filled.
    #[test]
    fn the_layout_editor_guesses_no_scalar_type_and_a_refusal_keeps_the_form() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert!(
            APP_JS.contains("var options = sdScalarTypes"),
            "the type picker is built from the served list"
        );
        assert!(
            APP_JS.contains("'<option value=\"\">no scalar type served</option>'"),
            "an empty served list is an empty picker and a refusal, never a guessed eighteen"
        );
        assert!(
            APP_JS.contains("function renderRefusal(boxId, status, text)"),
            "one refusal renderer for all three 409s — three would be three chances to get it wrong"
        );
        assert!(
            APP_JS.contains("still holds what you entered"),
            "a refusal that discarded the form would make the retry a retype"
        );
        // The refusal renderer must survive a body that is not the
        // structured shape: every other error in this file is plain text.
        assert!(
            APP_JS.contains("if (!body || !body.referenced_by) {"),
            "an unparseable refusal is rendered verbatim"
        );
    }

    /// The `.eap` editor's load-bearing details, as text guards.
    ///
    /// Each of these has a wrong version that renders and looks fine until
    /// somebody has a real file open: a wrapped line makes every gutter
    /// number and every band below it wrong; a second copy of the line
    /// height lands a band one line off; a per-line element id turns one
    /// file into hundreds of declared ids the guard cannot check.
    #[test]
    fn the_eap_editor_keeps_its_gutter_bands_and_text_in_step() {
        const APP_JS: &str = include_str!("../assets/app.js");
        const INDEX: &str = include_str!("../assets/index.html");
        const CSS: &str = include_str!("../assets/style.css");

        assert!(
            INDEX.contains("wrap=\"off\""),
            "wrap=off is load-bearing: a wrapped line makes one source line two rendered rows"
        );
        assert!(
            INDEX.contains("<textarea id=\"sd-eap-text\""),
            "a real textarea, so native caret, undo, IME and clipboard behaviour come free"
        );
        assert_eq!(
            CSS.matches("--eap-line:").count(),
            1,
            "one definition of the line height — two copies is how a band lands one line off"
        );
        assert!(
            !CSS.contains("background: var(--bg-surface-inset);\n  overflow: hidden;\n}\n\n.eap-gutter"),
            "the editor background must not be --bg-surface-inset, which stays dark in light theme"
        );
        assert!(
            APP_JS.contains(".filter(function (e) { return e.line > 0; })"),
            "a line-0 error gets no band, but still lists below the editor"
        );
        assert!(
            APP_JS.contains("eap-bands eap-stale"),
            "editing after a Check greys the bands rather than silently dropping them"
        );
        assert!(
            APP_JS.contains("text.setSelectionRange(offset,"),
            "an error row clicks through to its line on the real textarea"
        );
        // No id built by concatenation: per-file and per-error elements key
        // off `data-*`, which is what keeps `tests/element_ids.rs` able to
        // see every id this file declares.
        assert!(
            APP_JS.contains("data-eap-file=") && APP_JS.contains("data-eap-error="),
            "per-file and per-error elements key off data-*, never a built id"
        );
        assert!(
            !APP_JS.contains("\"sd-eap-line-\"") && !APP_JS.contains("sd-eap-error-\" +"),
            "no element id is built by concatenation"
        );
        // One wrapper, not a new one: the id guard hardcodes four names, so
        // an `eapEl()` would hide every lookup inside it.
        assert!(!APP_JS.contains("function eapEl("), "no new lookup wrapper");
    }

    /// The `RunProtocol` row re-hydrates **both** of its fields on load.
    ///
    /// This is the hole `embarch-ui` decision 17 records as having silently
    /// dropped monitor targets — one feature later, with two fields instead
    /// of one.
    #[test]
    fn a_run_protocol_row_reloads_both_of_its_fields() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert!(APP_JS.contains("base.protocol = a.protocol || \"\";"));
        assert!(APP_JS.contains("base.entryState = a.entry_state || \"\";"));
        assert!(
            APP_JS.contains("unknown until the file parses")
                || APP_JS.contains("did not parse"),
            "a file that did not parse reads as unreadable, never as a protocol with no states"
        );
        assert!(
            APP_JS.contains("not a state of "),
            "a renamed state keeps what was authored and says it is not there"
        );
        assert!(
            APP_JS.contains("is a terminal state of "),
            "a terminal entry state is refused before submit, from the served flag"
        );
    }

    /// **One adopt-and-render, three callers.**
    ///
    /// The sequence that repaints everything reading an `/actions` response
    /// existed three times and had already drifted — `sdEnterProject`
    /// repainted four of the six, so two pools stayed at their `index.html`
    /// placeholders until some unrelated call happened to run one of the
    /// other two. Found by clicking the real page. A renderer added to the
    /// one function is added everywhere.
    #[test]
    fn one_function_adopts_an_actions_response() {
        const APP_JS: &str = include_str!("../assets/app.js");
        assert_eq!(APP_JS.matches("function sdAdoptActions(data)").count(), 1);
        // Three callers, and no second copy of the assignment that starts it.
        assert_eq!(APP_JS.matches("sdAdoptActions(data);").count(), 3);
        assert_eq!(
            APP_JS.matches("sdRegistry = sdRegisteredActions();").count(),
            1,
            "a second copy of this line is a second copy of the sequence"
        );
        let body = {
            let start = APP_JS.find("function sdAdoptActions(data)").unwrap();
            let rest = &APP_JS[start..];
            &rest[..rest.find("\n  }\n").unwrap()]
        };
        for renderer in [
            "renderSdUnregistered()",
            "renderSdRegistered()",
            "renderSdLayouts()",
            "renderSdProtocolsCard()",
            "renderSdRows()",
            "renderSdTaps()",
        ] {
            assert!(body.contains(renderer), "{renderer} must repaint on an actions response");
        }
    }

    /// **`app.js` holds no copy of a served fact.** The positive guard above
    /// proves the server sends the right number; this proves the browser
    /// does not carry its own. Both are needed: a browser with a fallback
    /// renders a plausible wrong value on exactly the request that failed.
    #[test]
    fn app_js_holds_no_copy_of_a_served_limit_or_vocabulary() {
        const APP_JS: &str = include_str!("../assets/app.js");
        for literal in [
            // DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN
            "3072",
            // DEV_BENCH_MAX_STEPS_PER_STUDY, as a bare comparison
            "> 16",
            // A log level spelled in the browser rather than served.
            "\"Warn\"",
            "\"Debug\"",
            // A scalar type spelled in the browser rather than served.
            "\"u16le\"",
            "\"f32le\"",
        ] {
            assert!(
                !APP_JS.contains(literal),
                "app.js holds {literal}, which is a served fact — read it off the response"
            );
        }
    }

    /// `seal_crc` seals **all three** of a study's seals, not the two it was
    /// first written against.
    ///
    /// The regression this pins is invisible while `Study.protocols` is
    /// empty — `protocols_crc(&[])` is 0, which is also what an unsealed
    /// field holds — so the test hand-builds a study carrying one
    /// `ProtocolDef` and asserts the sealed value is the crate's own
    /// `protocols_crc` of it *and* that it is not 0. Asserting only the
    /// former would pass against a `seal_crc` that never touched the field.
    /// Same failure `embarch-api`'s `reseal_study` had (row 76).
    #[test]
    fn seal_crc_seals_all_three_including_protocols() {
        use embarch_study_designer::{
            ActiveState, ProtocolDef, StateDef, StateKind, TerminalOutcome,
        };

        let mut states = embarch_study_designer::bounded::Bounded::new();
        states
            .push(StateDef {
                name: heapless::String::try_from("go").unwrap(),
                kind: StateKind::Active(ActiveState {
                    on_enter: None,
                    on_event: heapless::Vec::new(),
                    on_timeout: None,
                }),
            })
            .unwrap();
        states
            .push(StateDef {
                name: heapless::String::try_from("done").unwrap(),
                kind: StateKind::Terminal(TerminalOutcome::Pass),
            })
            .unwrap();
        let def = ProtocolDef {
            name: heapless::String::try_from("bds").unwrap(),
            sources: heapless::Vec::new(),
            frames: heapless::Vec::new(),
            session: heapless::Vec::new(),
            states,
        };

        let mut study = build_study(
            "seal-test",
            RequirementsInput::any().build().unwrap(),
            &[],
            &ActionRegistry::default(),
            &[],
        )
        .unwrap();
        study.protocols.push(def).unwrap();
        study.protocols_crc = 0;

        seal_crc(&mut study).unwrap();

        let expected = embarch_study_designer::protocols_crc(&study.protocols).unwrap();
        assert_eq!(study.protocols_crc, expected);
        assert_ne!(
            study.protocols_crc, 0,
            "a study carrying a protocol must not seal to the empty-list value"
        );
    }

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

    /// `embarch-study-designer` decision 56: the response names
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

    /// `embarch-study-designer` decision 57: the same, one
    /// level up. A picker that groups by service (decision 17) needs a
    /// heading, and it comes from the identifier
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
    /// what every picker showed before `embarch-study-designer` decision 56.
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
    /// (decision 14): a firmware repo with no `embarch/`
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

    /// "New study" names nothing, so it must never be refused for a name it
    /// invented — the `409` an author actually hit read "'alpha-study'
    /// already exists", about the study they had open and had not asked to
    /// duplicate.
    #[test]
    fn a_unique_new_study_lands_beside_the_names_already_taken() {
        let scratch = Scratch::new("first-free-slug");
        let dir = scratch.0.join("studies");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(first_free_slug(&dir, "untitled-study").0, "untitled-study");
        std::fs::write(dir.join("untitled-study.json"), "{}").unwrap();
        assert_eq!(first_free_slug(&dir, "untitled-study").0, "untitled-study-2");
        std::fs::write(dir.join("untitled-study-2.json"), "{}").unwrap();
        let (slug, path) = first_free_slug(&dir, "untitled-study");
        assert_eq!(slug, "untitled-study-3");
        assert!(!path.exists(), "the path handed back is the one that is free");
    }

    /// The distinction the whole static-analysis panel rests on: "no
    /// extractor configured" and "an extractor ran and failed" are different
    /// answers, and the forced path returns the second rather than logging it
    /// and handing back the first. Collapsing them is how "static analysis
    /// found nothing" came to mean both "there is nothing" and "nobody
    /// looked".
    #[test]
    fn an_unrecognized_extractor_is_an_error_not_an_empty_extraction() {
        let core = Arc::new(
            embarch_core_client::CoreClient::new(
                &toml::from_str("base_url = \"http://127.0.0.1:1\"\n").unwrap(),
            )
            .unwrap(),
        );
        let scratch = Scratch::new("bad-extractor");
        let sd = StudyDesigner::new(
            Some(StudyDesignerConfig {
                firmware_repo_path: scratch.0.clone(),
                static_extractor: Some("not-an-extractor".to_string()),
            }),
            core,
        );
        let err = sd.force_static_extraction().expect_err("an unknown name must not pass");
        assert!(err.contains("not-an-extractor"), "the refusal must name what was asked for: {err}");
        assert!(
            err.contains(STATIC_EXTRACTOR_NAME),
            "...and what would have worked: {err}"
        );
        // Cached as `Some(None)` so the ordinary lazy path doesn't quietly
        // re-run an extractor that has just failed in front of a human.
        let cached = sd.0.static_gatt.lock().unwrap();
        assert_eq!(cached.as_ref().map(Option::is_none), Some(true));
    }

    /// No name configured is the ordinary state of a repo nobody has pointed
    /// an extractor at, and the forced path must not dress it up as a fault.
    #[test]
    fn forcing_with_no_extractor_configured_is_not_an_error() {
        let core = Arc::new(
            embarch_core_client::CoreClient::new(
                &toml::from_str("base_url = \"http://127.0.0.1:1\"\n").unwrap(),
            )
            .unwrap(),
        );
        let scratch = Scratch::new("force-no-extractor");
        let sd = StudyDesigner::new(
            Some(StudyDesignerConfig {
                firmware_repo_path: scratch.0.clone(),
                static_extractor: None,
            }),
            core,
        );
        assert!(sd.force_static_extraction().unwrap().is_none());
    }

    /// The cache is what makes the lazy path affordable and what would make
    /// an explicit "run the extractor" button a button that does nothing
    /// after its first press — so the forced path discards it first.
    #[test]
    fn forcing_discards_the_cache_rather_than_serving_it() {
        let core = Arc::new(
            embarch_core_client::CoreClient::new(
                &toml::from_str("base_url = \"http://127.0.0.1:1\"\n").unwrap(),
            )
            .unwrap(),
        );
        let scratch = Scratch::new("force-discards");
        let sd = StudyDesigner::new(
            Some(StudyDesignerConfig {
                firmware_repo_path: scratch.0.clone(),
                static_extractor: Some("not-an-extractor".to_string()),
            }),
            core,
        );
        // Pretend a previous extraction had found something: the lazy path
        // would serve this forever.
        *sd.0.static_gatt.lock().unwrap() = Some(Some(StaticGatt {
            services: Vec::new(),
            symbols: vec![(Uuid([0u8; 16]), "stale_symbol".to_string())],
            service_symbols: Vec::new(),
        }));
        assert!(sd.static_extraction().is_some(), "the lazy path serves the cache");
        assert!(sd.force_static_extraction().is_err(), "the forced path re-runs");
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
            build: None,
            outpost: None,
        }
    }

    /// The distinction `embarch-study-designer` decision 40 rests on: `"any"`
    /// is a deliberate answer and blank is the not-thought-about case. Turning
    /// blank into `"any"` here would erase it, so blank is refused with the
    /// message the UI shows.
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
            record_magic: Vec::new(),
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
        let (taps, decoders, _) = build_taps(
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

    // ---- GattNotify taps (`embarch-study-designer` decisions 52/55) ----

    #[test]
    fn a_gatt_tap_with_a_named_layout_resolves_it_into_the_study() {
        // The submitted `Study` carries the layout, not the name: Core cannot
        // read the firmware repo, so a study that named a layout without
        // carrying it would render nothing on any machine but this one.
        let (taps, decoders, _) = build_taps(
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

    fn gatt_tap_with_magic(name: &str, magic: &[u8]) -> TapInput {
        TapInput::GattNotify {
            name: name.to_string(),
            service_uuid: NUS_SERVICE.to_string(),
            characteristic_uuid: NUS_TX.to_string(),
            decoder: String::new(),
            record_magic: magic.to_vec(),
        }
    }

    /// The check's `stream_id` is the `id` `build_taps` assigned to the tap
    /// it rides on. Minting it in that loop rather than accepting it is what
    /// makes the two unable to disagree.
    #[test]
    fn a_record_check_carries_the_id_of_the_tap_it_rides_on() {
        let (taps, _, checks) = build_taps(
            &[gatt_tap("plain", NUS_TX, ""), gatt_tap_with_magic("framed", b"GWF1")],
            &monitoring_steps(),
            &structs_toml(),
        )
        .unwrap();
        assert_eq!(checks.len(), 1, "only the tap with a magic gets a check");
        assert_eq!(checks[0].stream_id, taps[1].id);
        assert_eq!(checks[0].stream_id, 1);
        let RecordFraming::MagicPrefixedCrc32Le { magic } = &checks[0].framing;
        assert_eq!(magic.as_slice(), b"GWF1");
    }

    /// An empty magic is **no check**, not a check that matches everything —
    /// the honest state for a payload whose framing nobody has declared.
    #[test]
    fn a_tap_with_no_magic_gets_no_check() {
        let (_, _, checks) =
            build_taps(&[gatt_tap("plain", NUS_TX, "")], &monitoring_steps(), &structs_toml())
                .unwrap();
        assert!(checks.is_empty());
    }

    /// Refused naming the tap, never truncated: a shortened magic finds
    /// different record boundaries, which is a check that measures the wrong
    /// thing and passes.
    #[test]
    fn an_over_long_magic_is_refused_naming_the_tap() {
        let too_long = vec![0xAA; MAX_RECORD_MAGIC_LEN + 1];
        let err = build_taps(
            &[gatt_tap_with_magic("framed", &too_long)],
            &monitoring_steps(),
            &structs_toml(),
        )
        .unwrap_err();
        assert!(err.contains("'framed'"), "{err}");
        assert!(err.contains(&MAX_RECORD_MAGIC_LEN.to_string()), "{err}");
        assert!(err.contains(&too_long.len().to_string()), "{err}");
    }

    /// A study written by hand, or saved before the sidecar existed, still
    /// loads its record magic back — off `record_checks` keyed by
    /// `stream_id`, which is the tap's own index. Same lookup-not-a-guess
    /// property the decoder round trip has.
    #[test]
    fn a_record_magic_comes_back_off_record_checks_with_no_sidecar() {
        let study = serde_json::json!({
            "streams": [{
                "id": 0,
                "name": "ppg",
                "source": { "GattNotify": {
                    "service_uuid": Uuid::parse(NUS_SERVICE).unwrap().0.to_vec(),
                    "characteristic_uuid": Uuid::parse(NUS_TX).unwrap().0.to_vec()
                }},
                "encoding": "Raw",
                "scope": "WholeStudy"
            }],
            "record_checks": [{
                "stream_id": 0,
                "framing": { "MagicPrefixedCrc32Le": { "magic": [0x47, 0x57, 0x46, 0x31] } }
            }]
        });
        match &taps_from_streams(&study)[0] {
            TapInput::GattNotify { record_magic, .. } => assert_eq!(record_magic, b"GWF1"),
            other => panic!("expected a GattNotify tap, got {other:?}"),
        }
    }

    /// What a saved study's JSON actually carries. `build_authored` is the
    /// one path both `run` and `save` take, so a key missing here is a key
    /// missing from the file *and* from the submitted study.
    #[test]
    fn an_authored_study_carries_its_log_level_and_record_checks() {
        let rows = vec![TableRow {
            name: "monitor".to_string(),
            action: RowAction::BuiltIn {
                targets: Vec::new(),
                which: BuiltInActionKind::GattMonitorStart,
                role: RoleChoice::Central,
                target_name: None,
                security_level: None,
                protocol: None,
                entry_state: None,
            },
            timeout_ms: 1_000,
            continue_on_fail: false,
            delay_before_ms: 0,
        }];
        let study = build_authored(
            "framed",
            &rows,
            &RequirementsInput::any(),
            &[gatt_tap_with_magic("ppg", b"GWF1")],
            &ActionRegistry::default(),
            &StructRegistry::default(),
            &[],
            Some(DevBenchLogLevel::Debug),
        )
        .unwrap();

        let json = serde_json::to_value(&study).unwrap();
        assert_eq!(json["dev_bench_log_level"], "Debug");
        assert_eq!(json["record_checks"][0]["stream_id"], 0);
        assert_eq!(
            json["record_checks"][0]["framing"]["MagicPrefixedCrc32Le"]["magic"],
            serde_json::json!([0x47, 0x57, 0x46, 0x31])
        );
    }

    /// An absent level leaves the crate's own argued default in place rather
    /// than this layer restating `Warn` — a second copy of that decision
    /// would stop tracking the first.
    #[test]
    fn an_absent_log_level_leaves_the_crates_default() {
        let study = build_authored(
            "quiet",
            &[],
            &RequirementsInput::any(),
            &[],
            &ActionRegistry::default(),
            &StructRegistry::default(),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(study.dev_bench_log_level, DevBenchLogLevel::default());
        assert_eq!(serde_json::to_value(&study).unwrap()["dev_bench_log_level"], "Warn");
    }

    #[test]
    fn a_gatt_tap_with_no_layout_is_raw_rather_than_guessed_at() {
        let (taps, decoders, _) =
            build_taps(&[gatt_tap("ppg", NUS_TX, "")], &monitoring_steps(), &structs_toml())
                .unwrap();
        assert!(decoders.is_empty());
        assert!(matches!(taps[0].encoding, StreamEncoding::Raw));
    }

    #[test]
    fn two_taps_sharing_a_layout_share_one_decoder_slot() {
        let (taps, decoders, _) = build_taps(
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
        // passes, and looks fine — one instance of the "nothing captured, no
        // error" family `embarch-study-designer` decisions 34/36/54/55 were
        // each opened by; decision 54 names the family, decision 55 describes
        // this exact case. Caught where the author can fix it.
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

    // ---- the auto-declared transcript tap (decision 15) ----

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
        let (mut streams, _, _) = build_taps(
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
    /// defect `embarch-study-designer` decision 40 closes.
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
            TapInput::GattNotify { name, service_uuid, characteristic_uuid, decoder, .. } => {
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

    // --- pre-flight -------------------------------------------------------

    /// A study under every cap produces no advisory. The comparison exists
    /// to be meaningful, not permanently true.
    #[test]
    fn a_small_study_is_over_no_advisory_cap() {
        let study = build_authored(
            "small",
            &[],
            &RequirementsInput::any(),
            &[],
            &ActionRegistry::default(),
            &StructRegistry::default(),
            &[],
            None,
        )
        .unwrap();
        let wire = embarch_study_designer::protocols_wire_len(&study.protocols).unwrap();
        assert!(advisories_for(&study, wire).is_empty());
    }

    /// Over the step cap: one sentence, naming this study's count and the
    /// bench's. Nothing in it says whether to run.
    #[test]
    fn a_study_over_the_step_cap_gets_one_sentence_and_no_verdict() {
        use embarch_study_designer::limits::DEV_BENCH_MAX_STEPS_PER_STUDY;
        let rows: Vec<TableRow> = (0..DEV_BENCH_MAX_STEPS_PER_STUDY + 1)
            .map(|i| TableRow {
                name: format!("s{i}"),
                action: RowAction::BuiltIn {
                    targets: Vec::new(),
                    which: BuiltInActionKind::GattDiscover,
                    role: RoleChoice::Central,
                    target_name: None,
                    security_level: None,
                    protocol: None,
                    entry_state: None,
                },
                timeout_ms: 1_000,
                continue_on_fail: false,
                delay_before_ms: 0,
            })
            .collect();
        let study = build_authored(
            "big",
            &rows,
            &RequirementsInput::any(),
            &[],
            &ActionRegistry::default(),
            &StructRegistry::default(),
            &[],
            None,
        )
        .unwrap();
        let wire = embarch_study_designer::protocols_wire_len(&study.protocols).unwrap();
        let advisories = advisories_for(&study, wire);
        assert_eq!(advisories.len(), 1, "{advisories:?}");
        assert!(advisories[0].contains(&(DEV_BENCH_MAX_STEPS_PER_STUDY + 1).to_string()));
        assert!(advisories[0].contains(&DEV_BENCH_MAX_STEPS_PER_STUDY.to_string()));
        for verdict in ["cannot", "refuse this", "will not run", "too many"] {
            assert!(!advisories[0].contains(verdict), "advisory reads as a gate: {advisories:?}");
        }
    }

    /// The wire-length advisory fires on the encoding, not on a count —
    /// which is the whole reason `protocols_wire_len` exists.
    #[test]
    fn the_wire_length_advisory_reads_the_encoded_size() {
        let study = build_authored(
            "small",
            &[],
            &RequirementsInput::any(),
            &[],
            &ActionRegistry::default(),
            &StructRegistry::default(),
            &[],
            None,
        )
        .unwrap();
        let over = embarch_study_designer::limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN + 1;
        let advisories = advisories_for(&study, over);
        assert_eq!(advisories.len(), 1, "{advisories:?}");
        assert!(advisories[0].contains(&over.to_string()));
        assert!(advisories[0].contains(
            &embarch_study_designer::limits::DEV_BENCH_MAX_PROTOCOLS_WIRE_LEN.to_string()
        ));
    }

    // --- running a saved study as it is ------------------------------------

    fn saved_study_with_protocol(entry_state: u8, states: &[(&str, bool)]) -> Study {
        let mut study = build_authored(
            "as-is",
            &[],
            &RequirementsInput::any(),
            &[],
            &ActionRegistry::default(),
            &StructRegistry::default(),
            &[],
            None,
        )
        .unwrap();
        let mut defs = embarch_study_designer::bounded::Bounded::new();
        for (name, terminal) in states {
            defs.push(embarch_study_designer::StateDef {
                name: HString::try_from(*name).unwrap(),
                kind: if *terminal {
                    embarch_study_designer::StateKind::Terminal(
                        embarch_study_designer::TerminalOutcome::Pass,
                    )
                } else {
                    embarch_study_designer::StateKind::Active(
                        embarch_study_designer::ActiveState {
                            on_enter: None,
                            on_event: HVec::new(),
                            on_timeout: None,
                        },
                    )
                },
            })
            .unwrap();
        }
        study
            .protocols
            .push(embarch_study_designer::ProtocolDef {
                name: HString::try_from("bds").unwrap(),
                sources: HVec::new(),
                frames: HVec::new(),
                session: HVec::new(),
                states: defs,
            })
            .unwrap();
        study
            .steps
            .push(Step {
                name: HString::try_from("handshake").unwrap(),
                action: Action::RunProtocol { protocol: 0, entry_state },
                timeout_ms: 30_000,
                continue_on_fail: false,
                delay_before_ms: 0,
            })
            .unwrap();
        study
    }

    /// The two index checks `validate_protocol` cannot make, because it
    /// never sees an `Action`. Both would otherwise reach a hand-written C
    /// array subscript on the bench.
    #[test]
    fn a_hand_written_study_gets_the_run_protocol_index_checks() {
        // In range and not terminal: accepted.
        assert!(preflight_saved(&saved_study_with_protocol(0, &[("go", false), ("done", true)]))
            .is_ok());

        // Entry state past the end.
        let err = preflight_saved(&saved_study_with_protocol(9, &[("go", false), ("done", true)]))
            .unwrap_err();
        assert!(err.contains("state 9"), "{err}");
        assert!(err.contains("handshake"), "{err}");

        // Entry state terminal.
        let err = preflight_saved(&saved_study_with_protocol(1, &[("go", false), ("done", true)]))
            .unwrap_err();
        assert!(err.contains("terminal"), "{err}");
        assert!(err.contains("'done'"), "{err}");

        // Protocol index past the end.
        let mut study = saved_study_with_protocol(0, &[("go", false), ("done", true)]);
        study.steps[0].action = Action::RunProtocol { protocol: 3, entry_state: 0 };
        let err = preflight_saved(&study).unwrap_err();
        assert!(err.contains("protocol 3"), "{err}");
    }

    /// `validate_protocol`'s own refusals reach the caller naming the
    /// protocol — a manifest with no terminal state can never finish.
    #[test]
    fn a_protocol_that_can_never_finish_is_refused_naming_it() {
        let err = preflight_saved(&saved_study_with_protocol(0, &[("go", false)])).unwrap_err();
        assert!(err.contains("'bds'"), "{err}");
        assert!(err.contains("terminal"), "{err}");
    }

    // --- the reference scan ---------------------------------------------

    fn repo_with_studies(tag: &str, files: &[(&str, serde_json::Value)]) -> (Scratch, StudyDesignerConfig) {
        let scratch = Scratch::new(tag);
        let dir = scratch.0.join("embarch").join("studies");
        std::fs::create_dir_all(&dir).unwrap();
        for (slug, value) in files {
            std::fs::write(
                dir.join(format!("{slug}.json")),
                serde_json::to_string_pretty(value).unwrap(),
            )
            .unwrap();
        }
        let config = StudyDesignerConfig {
            firmware_repo_path: scratch.0.clone(),
            static_extractor: None,
        };
        (scratch, config)
    }

    #[test]
    fn a_registered_action_is_found_by_the_step_that_names_it() {
        let (_scratch, project) = repo_with_studies(
            "refs-action",
            &[(
                "drain",
                serde_json::json!({
                    "name": "Ten hour drain",
                    "_embarch_ui_rows": [
                        { "name": "connect", "action": { "kind": "built_in", "which": "ble_connect" } },
                        { "name": "start-log", "action": { "kind": "registered", "name": "start_logging" } }
                    ]
                }),
            )],
        );
        let scan = scan_references(&project, RefKind::Action, "start_logging");
        assert!(scan.blocks());
        assert_eq!(scan.referenced_by.len(), 1);
        assert_eq!(scan.referenced_by[0].slug, "drain");
        assert_eq!(scan.referenced_by[0].name, "Ten hour drain");
        assert_eq!(scan.referenced_by[0].steps, vec!["start-log".to_string()]);

        // And an action nothing names is deletable.
        assert!(!scan_references(&project, RefKind::Action, "stop_logging").blocks());
    }

    /// **Both places a layout can appear.** A hand-written study has no
    /// sidecar, so `decoders[].name` is the only trace of it; a study whose
    /// tap names a layout that failed to resolve has no `decoders` entry, so
    /// the sidecar is the only trace. Either check alone misses a real
    /// reference.
    #[test]
    fn a_layout_referenced_only_through_decoders_is_still_found() {
        let (_scratch, project) = repo_with_studies(
            "refs-layout",
            &[
                (
                    "hand-written",
                    serde_json::json!({
                        "name": "hand written",
                        "decoders": [{ "name": "ppg_packet", "header": [], "repeat": [] }]
                    }),
                ),
                (
                    "sidecar-only",
                    serde_json::json!({
                        "name": "sidecar only",
                        "_embarch_ui_taps": [
                            { "kind": "gatt_notify", "name": "ppg", "decoder": "ppg_packet" }
                        ]
                    }),
                ),
            ],
        );
        let scan = scan_references(&project, RefKind::Layout, "ppg_packet");
        assert_eq!(scan.referenced_by.len(), 2, "{scan:?}");
        assert_eq!(scan.referenced_by[0].slug, "hand-written");
        assert_eq!(
            scan.referenced_by[0].steps,
            vec!["the study carries this layout".to_string()]
        );
        assert_eq!(scan.referenced_by[1].steps, vec!["tap 'ppg'".to_string()]);
    }

    #[test]
    fn a_protocol_is_found_through_the_row_and_through_the_carried_list() {
        let (_scratch, project) = repo_with_studies(
            "refs-protocol",
            &[(
                "bds",
                serde_json::json!({
                    "name": "BDS download",
                    "protocols": [{ "name": "bds_batch_download" }],
                    "_embarch_ui_rows": [
                        {
                            "name": "handshake",
                            "action": {
                                "kind": "built_in",
                                "which": "run_protocol",
                                "protocol": "bds_batch_download",
                                "entry_state": "start"
                            }
                        }
                    ]
                }),
            )],
        );
        let scan = scan_references(&project, RefKind::Protocol, "bds_batch_download");
        assert_eq!(scan.referenced_by.len(), 1);
        assert_eq!(
            scan.referenced_by[0].steps,
            vec!["handshake".to_string(), "the study carries this protocol".to_string()]
        );
    }

    /// **A file this cannot read is reported as unscannable, never as "no
    /// references".** A directory it cannot fully read is not permission to
    /// delete: the file it could not parse might be the one study using the
    /// thing about to disappear.
    #[test]
    fn an_unparseable_study_blocks_a_delete_rather_than_reading_as_no_references() {
        let (scratch, project) = repo_with_studies("refs-broken", &[]);
        std::fs::write(
            scratch.0.join("embarch").join("studies").join("broken.json"),
            "{ this is not json",
        )
        .unwrap();

        let scan = scan_references(&project, RefKind::Action, "anything");
        assert!(scan.referenced_by.is_empty());
        assert_eq!(scan.unscannable, vec!["broken.json is not valid JSON".to_string()]);
        assert!(scan.blocks(), "an unscannable file must refuse the delete");
    }

    #[test]
    fn a_missing_studies_directory_is_an_empty_scan() {
        let scratch = Scratch::new("refs-none");
        let project = StudyDesignerConfig {
            firmware_repo_path: scratch.0.clone(),
            static_extractor: None,
        };
        let scan = scan_references(&project, RefKind::Protocol, "bds");
        assert!(!scan.blocks());
    }

    /// The `409` body is the one structured error in this file, because its
    /// payload is a list the dialog renders as a table.
    #[test]
    fn the_refusal_body_names_the_studies_as_a_list() {
        let scan = ReferenceScan {
            referenced_by: vec![StudyReference {
                slug: "drain".to_string(),
                name: "Ten hour drain".to_string(),
                steps: vec!["start-log".to_string()],
            }],
            unscannable: Vec::new(),
        };
        let json = serde_json::to_value(serde_json::json!({
            "error": format!(
                "registered action 'x' is still used by {} saved study",
                scan.referenced_by.len()
            ),
            "referenced_by": &scan.referenced_by,
            "unscannable": &scan.unscannable,
        }))
        .unwrap();
        assert_eq!(json["referenced_by"][0]["slug"], "drain");
        assert_eq!(json["referenced_by"][0]["name"], "Ten hour drain");
        assert_eq!(json["referenced_by"][0]["steps"][0], "start-log");
    }

    // --- the protocol repo ----------------------------------------------

    const ONE_PROTOCOL: &str =
        "protocol bds {\n    state start {\n        on_timeout 1000ms: goto done\n    }\n\n    state done outcome: pass\n}\n";

    fn repo_with_protocols(tag: &str, files: &[(&str, &str)]) -> Scratch {
        let scratch = Scratch::new(tag);
        let dir = scratch.0.join("embarch").join("protocols");
        std::fs::create_dir_all(&dir).unwrap();
        for (stem, text) in files {
            std::fs::write(dir.join(format!("{stem}.eap")), text).unwrap();
        }
        scratch
    }

    /// A file that did not parse carries its error and does not take the
    /// listing down with it; the file beside it still offers its protocol.
    #[test]
    fn the_protocols_listing_reports_a_bad_file_beside_the_good_ones() {
        let scratch = repo_with_protocols(
            "protocols-mixed",
            &[("bds", ONE_PROTOCOL), ("broken", "protocol oops {\n  state go {\n")],
        );
        let repo = embarch_study_designer::eap_repo::scan(&scratch.0).unwrap();
        let out = protocols_response(&repo);

        assert_eq!(out.files.len(), 2);
        let bds = out.files.iter().find(|f| f.stem == "bds").unwrap();
        assert_eq!(bds.protocols.len(), 1);
        assert!(bds.errors.is_empty());
        let json = serde_json::to_value(&bds.protocols).unwrap();
        assert_eq!(json[0]["name"], "bds");
        assert_eq!(json[0]["file"], "bds");
        assert_eq!(json[0]["states"][0]["name"], "start");
        assert_eq!(json[0]["states"][0]["terminal"], false);
        assert_eq!(json[0]["states"][1]["terminal"], true);
        assert_eq!(json[0]["states"][1]["outcome"], "pass");

        let broken = out.files.iter().find(|f| f.stem == "broken").unwrap();
        assert!(broken.protocols.is_empty(), "the file did not parse, so it offers nothing");
        assert_eq!(broken.errors.len(), 1, "one parse error, never a list");
        assert!(broken.errors[0].line > 0, "the editor bands this line");
        assert!(broken.errors[0].message.starts_with("line "), "{:?}", broken.errors[0]);
        assert!(out.duplicate_names.is_empty());

        // **The listing carries each file's text.** Without it the editor
        // assigns `undefined` to a textarea, which yields the nine-character
        // string "undefined" and reports a syntax error on line 1 — found by
        // driving the real page, and invisible to every other test here.
        assert_eq!(bds.text, ONE_PROTOCOL);
        assert_eq!(broken.text, "protocol oops {\n  state go {\n");
    }

    /// A block that **parsed but did not resolve** is listed with its name
    /// and no states, and a file that did not parse at all is listed by
    /// stem. They are different facts and a row naming one should be able to
    /// say which.
    #[test]
    fn an_unresolved_block_is_listed_and_an_unparsed_file_is_named() {
        // A block with a `goto` nothing declares: it parses, it does not
        // resolve.
        let unresolved =
            "protocol broken {\n    state go {\n        on_timeout 1000ms: goto nowhere\n    }\n\n    state done outcome: pass\n}\n";
        let scratch = repo_with_protocols(
            "protocols-unresolved",
            &[("ok", ONE_PROTOCOL), ("broken", unresolved), ("garbage", "protocol x {\n")],
        );
        let repo = embarch_study_designer::eap_repo::scan(&scratch.0).unwrap();

        let summaries = protocol_summaries(&repo);
        let json = serde_json::to_value(&summaries).unwrap();
        let broken = json
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "broken")
            .expect("an unresolved block is listed, not omitted");
        assert_eq!(broken["resolved"], false);
        assert_eq!(broken["states"].as_array().unwrap().len(), 0);
        assert_eq!(broken["file"], "broken");

        let ok = json.as_array().unwrap().iter().find(|p| p["name"] == "bds").unwrap();
        assert_eq!(ok["resolved"], true);

        // A file that did not parse declares nothing nameable, so it is
        // reported by stem instead.
        assert_eq!(unparsed_files(&repo), vec!["garbage".to_string()]);
    }

    #[test]
    fn a_name_declared_by_two_files_is_reported_repo_wide() {
        let scratch =
            repo_with_protocols("protocols-dup", &[("a", ONE_PROTOCOL), ("b", ONE_PROTOCOL)]);
        let repo = embarch_study_designer::eap_repo::scan(&scratch.0).unwrap();
        let out = protocols_response(&repo);
        assert_eq!(out.duplicate_names.len(), 1);
        assert_eq!(out.duplicate_names[0].name, "bds");
        assert_eq!(out.duplicate_names[0].files, vec!["a".to_string(), "b".to_string()]);
    }
}

// ---- projects (decision 14) -------------------------------------------------

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
    /// (`embarch-study-designer` decision 35's registry).
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
    /// (`embarch-study-designer` decision 38), server-side.
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
pub struct StaticAnalysisRequest {
    /// The extractor to run, when the panel's field has been edited. `None`
    /// leaves the project's configured one alone; `Some("")` clears it.
    ///
    /// Adopted into the open project rather than used for this one call: an
    /// extractor picked here is what every *other* surface — the merged
    /// action list, the characteristic names, the tap pickers — then reads
    /// through, and a name that only applied to the button that ran it would
    /// name one thing on this panel and nothing anywhere else.
    #[serde(default)]
    static_extractor: Option<String>,
}

/// One characteristic a static extraction found, as the panel renders it.
#[derive(Debug, Serialize)]
pub struct StaticCharacteristic {
    uuid: String,
    /// The C identifier it was declared under, where the extraction
    /// recovered one (`embarch-study-designer` decision 56).
    name: Option<GattName>,
    /// The ATT properties byte **as the source declares it** — a
    /// compile-time assertion about the firmware, not an observation off a
    /// board. The Detected-characteristics chips already distinguish the two
    /// and this panel says so in words.
    properties: u8,
}

#[derive(Debug, Serialize)]
pub struct StaticService {
    uuid: String,
    name: Option<GattName>,
    characteristics: Vec<StaticCharacteristic>,
}

/// What `POST /api/study-designer/static-analysis` answers.
///
/// `configured: false` with no error is the ordinary state of a repo nobody
/// has pointed an extractor at, and is deliberately not an error — see
/// [`run_static_extraction`].
#[derive(Debug, Serialize)]
pub struct StaticAnalysisResponse {
    extractor: Option<String>,
    configured: bool,
    error: Option<String>,
    services: Vec<StaticService>,
    characteristic_count: usize,
}

/// Runs the firmware repo's static GATT extractor on demand and reports what
/// it read out of the source.
///
/// This exists because the extraction was, until now, only ever a *side
/// effect*: it ran lazily the first time something else needed a name, cached
/// for the life of the project, and reported its failures to a log nobody
/// running the UI is reading. An engineer who wanted to know whether reading
/// the firmware source had found anything had to infer it from whether some
/// other panel's chips looked right.
pub async fn api_static_analysis(
    State(state): State<crate::AppState>,
    Json(req): Json<StaticAnalysisRequest>,
) -> axum::response::Response {
    let sd = state.study_designer;
    let Some(project) = sd.project() else { return not_configured() };

    if let Some(requested) = req.static_extractor.as_deref() {
        let requested = requested.trim();
        let requested = (!requested.is_empty()).then(|| requested.to_string());
        if requested != project.static_extractor {
            let config = StudyDesignerConfig {
                firmware_repo_path: project.firmware_repo_path.clone(),
                static_extractor: requested.clone(),
            };
            // The full project switch, not a field poke: changing which
            // extractor runs invalidates the live GATT table too, for the
            // same reason opening a different repo does — every name on
            // screen was resolved through the old one.
            sd.open_project(config);
            remember_recent_project(&RecentProject {
                path: project.firmware_repo_path.to_string_lossy().into_owned(),
                static_extractor: requested,
            });
        }
    }

    let extractor = sd.project().and_then(|p| p.static_extractor.clone());
    let (extraction, error) = match sd.force_static_extraction() {
        Ok(found) => (found, None),
        Err(e) => (None, Some(e)),
    };

    let names = match &extraction {
        Some(extraction) => GattNameBook::new()
            .with_symbols(extraction.symbols.clone())
            .with_service_symbols(extraction.service_symbols.clone()),
        None => GattNameBook::new(),
    };
    let services: Vec<StaticService> = extraction
        .iter()
        .flat_map(|extraction| extraction.services.iter())
        .map(|service| StaticService {
            uuid: service.uuid.to_hyphenated().to_string(),
            name: names.service(service.uuid),
            characteristics: service
                .characteristics
                .iter()
                .map(|chrc| StaticCharacteristic {
                    uuid: chrc.uuid.to_hyphenated().to_string(),
                    name: names.get(chrc.uuid),
                    properties: chrc.properties,
                })
                .collect(),
        })
        .collect();

    Json(StaticAnalysisResponse {
        configured: extractor.is_some(),
        extractor,
        error,
        characteristic_count: services.iter().map(|s| s.characteristics.len()).sum(),
        services,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct NewStudyRequest {
    name: String,
    /// Take `name` as a *base* and land on the first free slug after it —
    /// `untitled-study`, `untitled-study-2`, and so on — rather than
    /// refusing a name that is taken.
    ///
    /// The refusal below is not weakened by this and is not optional: it
    /// exists so a deliberately typed name never silently replaces
    /// somebody's work, and a caller that states a name still gets it.
    /// This flag is for the other case — "start a blank study", where the
    /// caller named nothing and being told that the name it invented is
    /// taken is an error message about a decision nobody made.
    #[serde(default)]
    unique: bool,
}

/// Creates a new, empty, **immediately valid and immediately runnable**
/// saved study in the open project.
///
/// Everything a `Study` needs in order to round-trip is supplied here rather
/// than left for the author to discover on their first save: `requires` is
/// mandatory with no serde default, so it is written as an explicit
/// `REQUIREMENT_ANY` on both fields (`embarch-study-designer` decision 40
/// — "I don't care which build" is a real answer that has to be
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
    let (slug, path) = if req.unique {
        // Named after the slug it landed on, not after the base it was asked
        // for: two files both calling themselves `untitled-study` would show
        // the same name in the Study name box, and a save from the second
        // one writes to the first one's slug — the overwrite the `409`
        // below exists to prevent, arrived at from the other side.
        first_free_slug(&dir, &slug)
    } else {
        let path = dir.join(format!("{slug}.json"));
        if path.exists() {
            return (
                StatusCode::CONFLICT,
                format!("'{slug}' already exists — open it, or pick another name"),
            )
                .into_response();
        }
        (slug, path)
    };
    // The name the study carries is the one it can be saved back under.
    let name = if req.unique { slug.clone() } else { req.name.clone() };
    let registry = match sd.registry() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let study = match build_authored(
        &name,
        &[],
        &RequirementsInput::any(),
        &[],
        &registry,
        &StructRegistry::default(),
        // A brand-new study has no rows, so no row names a protocol and
        // there is nothing to resolve against. Reading the repo's `.eap`
        // files here would make creating an empty study fail on a repo whose
        // protocols are mid-edit.
        &[],
        // And it states no log level, so the crate's default stands.
        None,
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
        "name": name,
        "path": path.to_string_lossy(),
        "steps": 0,
    }))
    .into_response()
}
