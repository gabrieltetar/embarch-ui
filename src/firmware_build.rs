//! Building and flashing a study's own DUT firmware before it runs
//! (decision 11, reversed 2026-09-18).
//!
//! # Why this exists here at all
//!
//! Decision 11 as first written said reflash was terminal-only, and named
//! the reason honestly: `embarch-api` owned the project config and the build
//! machinery, `embarch-ui` cannot depend on `embarch-api`, and duplicating
//! ~4,300 lines of Zephyr build code here was worse than naming the gap. The
//! third option it did not consider was moving the machinery somewhere both
//! could reach, which is what `embarch-firmware-build` now is. So this
//! module is not a second implementation of anything: it is the same
//! `config`/`resolve`/`build` `embarch-api`'s `reflash.rs` calls, sequenced
//! the same way.
//!
//! # The sequence, and why each step is where it is
//!
//! **check -> build -> flash -> reset -> `POST /study`**, which is
//! `reflash.rs`'s own order plus the reset it learned the hard way
//! (`embarch-decision-reversals.md` row 75: Core's flash halts the core
//! rather than starting it, so a board that is never reset keeps executing
//! the image that was there before, and a run then records a successful
//! flash while testing the old firmware).
//!
//! **The flash still goes over HTTP to Core.** Only the build is local. That
//! keeps this crate's standing rule — every hardware-adjacent call goes
//! through `embarch-core-client`, never through in-process hardware code
//! (decision 5) — exactly as it was: a build is `west` in a subprocess and
//! files on disk, and nothing here links `probe-rs` or `serialport`.
//!
//! **The tree is never moved.** `reflash.rs`'s load-bearing constraint is a
//! property of the machinery and it survives the move: nothing in
//! `embarch-firmware-build` spawns `git` for anything but a read. A study
//! whose `requires.firmware_version` names a revision the tree is not at is
//! a refusal naming both, never a checkout.
//!
//! # Where the build log goes
//!
//! Two places, because the owner asked for two and they are different
//! questions. Live, each line is broadcast to whoever is watching the run,
//! so a Zephyr build is not tens of seconds of silence. Durably, the whole
//! log is written to a file under the per-user data directory beside
//! `embarch-api`'s own rolling logfile — which the Debug tab already reads
//! directly, for the reason decision 13 gives: a local file whose writer is
//! this same machine is not the case decision 7's never-read-a-logfile rule
//! is about.
//!
//! **Local, not on Core's disk.** The build ran here; Core never saw it. A
//! route to upload it would make provenance complete at the cost of a new
//! on-disk artifact and a retention rule, and what actually establishes
//! which firmware a run used is the outpost header Core reads in its own
//! pre-flight, not the text of a compiler's output.

use anyhow::{Context, Result};
use embarch_firmware_build::{build as fwbuild, config as fwconfig, resolve};
use embarch_study_designer::study::BuildSpec;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many build logs are kept on disk before the oldest is dropped.
///
/// A build log is evidence about a run that has already happened, and the
/// runs that matter are recent ones; an unbounded directory is a slow leak
/// nobody notices. Fifty is well past a day's iterating and is still under
/// a few megabytes at the cap each file is truncated to.
const MAX_KEPT_LOGS: usize = 50;

/// `<per-user data dir>/embarch/ui/builds`.
///
/// Beside `embarch-api`'s `api/logs` rather than inside it: the two rotate
/// independently and one of them is a daily rolling file while these are
/// per-build, so sharing a directory would make `api_log`'s
/// latest-by-name rule pick up files it does not own.
pub fn log_dir() -> Result<PathBuf> {
    Ok(embarch_core_client::user_dirs::user_data_dir()?
        .join("ui")
        .join("builds"))
}

/// One entry in the durable build-log list.
#[derive(Debug, Clone, Serialize)]
pub struct BuildLogEntry {
    /// The file's own name, which is also how it is fetched back. Sortable:
    /// it starts with the UTC millisecond the build began.
    pub id: String,
    pub started_utc_ms: u64,
    pub project: String,
    pub descriptor: String,
    pub ok: bool,
    pub bytes: u64,
}

/// Where `embarch-api`'s project config is read from.
///
/// **Resolved per call, not held.** The config is a file an engineer writes
/// and this process only reads (the posture decision 14 already takes toward
/// it), so an edit takes effect on the next Build card open rather than on
/// the next restart.
///
/// The env var is the same one `embarch-api` itself honours, which is the
/// point: a bench that has configured `embarch-api` has configured this, and
/// a second config file naming the same projects is a second thing to keep
/// in step.
pub fn config_path(configured: Option<&Path>) -> Option<PathBuf> {
    configured
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("EMBARCH_API_CONFIG").map(PathBuf::from))
}

/// What the Build card can offer for the currently open firmware repo.
///
/// `available: false` with a `reason` is a first-class answer rather than an
/// error, and it is the common one on a bench that has never configured
/// `embarch-api`: the Study Designer works perfectly well without a build
/// toggle, and a `500` from a survey call would take the whole tab down over
/// a feature the engineer may not want.
#[derive(Debug, Serialize)]
pub struct BuildSurvey {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    /// The configured project whose `source_path` *is* the open repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// `resolve::list_targets`' own object: the targets, the snippets each
    /// app declares, and the project's configured defaults. Passed through
    /// rather than reshaped — the picker is a view of what the resolver will
    /// accept, and a second shape here is a second thing to keep in step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<serde_json::Value>,
    /// The outpost header flags, in bit order, so the mode picker is built
    /// from the one table rather than from a list typed into `app.js`.
    pub flags: Vec<&'static str>,
}

fn unavailable(reason: impl Into<String>, config_path: Option<&Path>) -> BuildSurvey {
    BuildSurvey {
        available: false,
        reason: Some(reason.into()),
        config_path: config_path.map(|p| p.display().to_string()),
        project: None,
        targets: None,
        flags: flag_names(),
    }
}

fn flag_names() -> Vec<&'static str> {
    embarch_study_designer::outpost::HeaderFlags::NAMED
        .iter()
        .map(|(_, name)| *name)
        .collect()
}

/// Which configured project has `repo` as its `source_path`.
///
/// **Matched by path, not named by the engineer.** The Study Designer
/// already knows which firmware repo is open (decision 14) and
/// `embarch-api`'s config already says which repo each project is; asking a
/// second time would be asking the engineer to keep two answers in step, and
/// the failure mode of getting it wrong is building a different repo than
/// the one whose studies are on screen.
///
/// Canonicalised on both sides, because the same directory reached through
/// a symlink, a `..`, or a differently-cased Windows path is the same
/// project and a string compare would say otherwise.
fn project_for_repo<'a>(config: &'a fwconfig::Config, repo: &Path) -> Option<&'a fwconfig::ProjectConfig> {
    let wanted = repo.canonicalize().ok()?;
    config
        .projects
        .iter()
        .find(|p| p.source_path.canonicalize().is_ok_and(|p| p == wanted))
}

pub fn survey(configured: Option<&Path>, repo: &Path) -> BuildSurvey {
    let Some(path) = config_path(configured) else {
        return unavailable(
            "no embarch-api config is configured, so there are no projects to build. Set \
             EMBARCH_API_CONFIG, or add `[build] config_path = \"…\"` to this UI's own config.",
            None,
        );
    };

    let config = match fwconfig::Config::load_from_path(&path) {
        Ok(c) => c,
        Err(e) => return unavailable(format!("{e:#}"), Some(&path)),
    };

    let Some(project) = project_for_repo(&config, repo) else {
        let known: Vec<String> = config
            .projects
            .iter()
            .map(|p| format!("{} ({})", p.name, p.source_path.display()))
            .collect();
        return unavailable(
            format!(
                "the open firmware repo ({}) is not one of the projects in {} — so nothing here \
                 knows how to build it. Configured projects: {}",
                repo.display(),
                path.display(),
                if known.is_empty() { "(none)".to_string() } else { known.join("; ") }
            ),
            Some(&path),
        );
    };

    match resolve::list_targets(project) {
        Ok(targets) => BuildSurvey {
            available: true,
            reason: None,
            config_path: Some(path.display().to_string()),
            project: Some(project.name.clone()),
            targets: Some(targets),
            flags: flag_names(),
        },
        Err(e) => unavailable(
            format!("project '{}' is configured but its targets could not be scanned: {e:#}", project.name),
            Some(&path),
        ),
    }
}

/// What a completed build and flash produced.
/// Where a build's progress lines go while it runs: `(stream, line)`, where
/// `stream` is `"stdout"`, `"stderr"` or `"info"` — the last for this
/// module's own narration of the steps around the compiler's output.
pub type ProgressSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct FlashedFirmware {
    /// What `derive_version` read off the tree — the value a run reports as
    /// having flashed, and the one a successful build writes back into the
    /// study's `requires.firmware_version`.
    pub version: String,
    pub artifact_path: String,
    pub descriptor: serde_json::Value,
    /// The durable log's id, so a failure can point at it.
    pub log_id: String,
}

/// check -> build -> flash -> reset, with every line handed to `on_line` as
/// it arrives and the whole log written to disk at the end.
///
/// `required_firmware` is the study's own `requires.firmware_version`, and
/// the check against it happens **before the build**, deliberately: a
/// mismatch means the working tree is not at the revision the study asks
/// for, and building it anyway would burn a minute to produce the wrong
/// image. The failure message says what it is and leaves the tree alone.
#[allow(clippy::too_many_arguments)]
pub async fn build_and_flash(
    core: &embarch_core_client::CoreClient,
    locks: &fwbuild::BuildLocks,
    configured_config: Option<&Path>,
    repo: &Path,
    spec: &BuildSpec,
    required_firmware: &str,
    allow_version_mismatch: bool,
    on_line: ProgressSink,
) -> Result<FlashedFirmware> {
    let path = config_path(configured_config)
        .context("no embarch-api config is configured, so there are no projects to build")?;
    let config = fwconfig::Config::load_from_path(&path)?;
    let project = project_for_repo(&config, repo).with_context(|| {
        format!(
            "the open firmware repo ({}) is not one of the projects in {}",
            repo.display(),
            path.display()
        )
    })?;

    let command = project
        .version_command
        .clone()
        .unwrap_or_else(embarch_core_client::version::default_version_command);
    let version =
        embarch_core_client::version::derive_version(&project.source_path, &command).await?;
    on_line("info", &format!("working tree is at {version}"));

    if !embarch_study_designer::requirement_satisfied(required_firmware, &version)
        && !allow_version_mismatch
    {
        anyhow::bail!(
            "this study requires DUT firmware '{required_firmware}' and the working tree is at \
             '{version}'. Nothing was built and the board was not touched — the tree is where it \
             was. Move the tree yourself, change what the study requires, or tick \"run anyway\"."
        );
    }

    // Owned, because `Selection` borrows and the `heapless` strings inside
    // the spec are not `&str` of the right lifetime.
    let snippets: Vec<String> = spec.snippets.iter().map(|s| s.as_str().to_string()).collect();
    let extra_args: Vec<String> = spec.extra_args.iter().map(|s| s.as_str().to_string()).collect();
    let selection = resolve::Selection {
        board: spec.board.as_ref().map(|s| s.as_str()),
        variant: spec.variant.as_ref().map(|s| s.as_str()),
        revision: spec.revision.as_ref().map(|s| s.as_str()),
        app: spec.app.as_ref().map(|s| s.as_str()),
        snippets: &snippets,
        extra_args: &extra_args,
    };

    let resolved = resolve::resolve(project, selection, core).await?;
    on_line(
        "info",
        &format!(
            "building {} with: {}",
            resolved.descriptor,
            resolved.plan.command.join(" ")
        ),
    );

    let collected = Arc::new(std::sync::Mutex::new(String::new()));
    let sink: fwbuild::LineSink = {
        let on_line = on_line.clone();
        let collected = collected.clone();
        Arc::new(move |stream, line| {
            let kind = match stream {
                fwbuild::BuildStream::Stdout => "stdout",
                fwbuild::BuildStream::Stderr => "stderr",
            };
            if let Ok(mut buf) = collected.lock() {
                buf.push_str(line);
                buf.push('\n');
            }
            on_line(kind, line);
        })
    };

    let started_utc_ms = crate::firmware_build::now_utc_ms();
    let built = locks.run_build_streaming(&resolved.plan, sink).await;

    let log_body = collected.lock().map(|b| b.clone()).unwrap_or_default();
    let ok = built.as_ref().map(|b| b.ready_to_flash()).unwrap_or(false);
    let log_id = write_log(
        started_utc_ms,
        &project.name,
        &resolved.descriptor,
        ok,
        &log_body,
    )
    .unwrap_or_else(|e| {
        // A log that could not be written is not a build that failed. Said
        // out loud in the live stream rather than swallowed, so a reader
        // looking for it later in the Debug tab knows why it is not there.
        on_line("info", &format!("the build log could not be written to disk: {e:#}"));
        String::new()
    });

    let built = built.with_context(|| format!("build failed for '{}'", project.name))?;
    if !built.ready_to_flash() {
        anyhow::bail!(
            "the build for '{}' did not produce a fresh artifact, so nothing was flashed. \
             {}",
            project.name,
            if built.timed_out {
                "It timed out.".to_string()
            } else {
                format!("west exited {:?}.", built.exit_code)
            }
        );
    }

    let artifact_path = built.artifact_path.display().to_string();
    on_line("info", &format!("flashing {artifact_path}"));
    core.flash(
        &resolved.chip,
        &artifact_path,
        &resolved.flash_format,
        resolved.base_address.as_deref(),
        resolved.probe_serial.as_deref(),
        false,
    )
    .await
    .with_context(|| format!("flash failed for '{}'", project.name))?;

    // Reversals row 75: Core's flash halts the core rather than starting it,
    // so without this the board keeps running the image that was there
    // before and the run tests the wrong firmware while reporting the right
    // one. It is also what puts a power-on outpost header on the wire for
    // Core's own pre-flight to read.
    on_line("info", "resetting the DUT so it runs what was just flashed");
    core.reset(&resolved.chip, resolved.probe_serial.as_deref())
        .await
        .with_context(|| format!("reset after flash failed for '{}'", project.name))?;

    Ok(FlashedFirmware {
        version,
        artifact_path,
        descriptor: resolved.descriptor,
        log_id,
    })
}

pub fn now_utc_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The filename a build's log is stored and fetched under.
///
/// Starts with the start time so the directory sorts chronologically by
/// name, the same trick `api_log`'s daily files use, and carries the project
/// so a listing is readable without opening anything.
fn log_name(started_utc_ms: u64, project: &str, ok: bool) -> String {
    let project: String = project
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!(
        "{started_utc_ms:013}-{project}-{}.log",
        if ok { "ok" } else { "failed" }
    )
}

fn write_log(
    started_utc_ms: u64,
    project: &str,
    descriptor: &serde_json::Value,
    ok: bool,
    body: &str,
) -> Result<String> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let name = log_name(started_utc_ms, project, ok);

    // A header, so a log read weeks later says what it was a build of. Kept
    // inside the file rather than in a sidecar: one file is one build, and a
    // sidecar is a second thing that can go missing.
    let header = format!(
        "# embarch-ui build log\n# project: {project}\n# target: {descriptor}\n\
         # started_utc_ms: {started_utc_ms}\n# result: {}\n\n",
        if ok { "ok" } else { "failed" }
    );
    std::fs::write(dir.join(&name), format!("{header}{body}"))
        .with_context(|| format!("failed to write the build log to {}", dir.display()))?;

    prune(&dir);
    Ok(name)
}

/// Drops the oldest logs past [`MAX_KEPT_LOGS`]. Best-effort: a directory
/// that cannot be pruned is a directory that grows, which is worse than
/// tidy and much better than a build that fails because of housekeeping.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    if names.len() <= MAX_KEPT_LOGS {
        return;
    }
    names.sort();
    for path in names.iter().take(names.len() - MAX_KEPT_LOGS) {
        let _ = std::fs::remove_file(path);
    }
}

/// Every stored build log, newest first.
///
/// **An absent directory is an empty list, not an error.** Opening the Debug
/// tab on a bench that has never built anything is the normal first
/// experience, not a fault.
pub fn list_logs() -> Vec<BuildLogEntry> {
    let Ok(dir) = log_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<BuildLogEntry> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|ext| ext != "log") {
                return None;
            }
            let id = path.file_name()?.to_str()?.to_string();
            let bytes = e.metadata().ok().map(|m| m.len()).unwrap_or(0);
            // Parsed back out of the name rather than re-read from the
            // file's header: a listing must not cost one open per entry.
            let (started_utc_ms, project, ok) = {
                let mut parts = id.trim_end_matches(".log").splitn(2, '-');
                let started = parts.next()?.parse::<u64>().ok()?;
                let rest = parts.next().unwrap_or("");
                let (project, status) = rest.rsplit_once('-').unwrap_or((rest, "failed"));
                (started, project.to_string(), status == "ok")
            };
            Some(BuildLogEntry {
                id,
                started_utc_ms,
                project,
                descriptor: String::new(),
                ok,
                bytes,
            })
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.started_utc_ms));
    out
}

/// One stored build log's text.
///
/// `id` is checked to be exactly a name this module writes — no separators,
/// no `..`, and the right extension — because it arrives from a URL.
pub fn read_log(id: &str) -> Result<String> {
    if id.is_empty()
        || !id.ends_with(".log")
        || id.contains(['/', '\\'])
        || id.contains("..")
    {
        anyhow::bail!("'{id}' is not a build log id");
    }
    let path = log_dir()?.join(id);
    std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_name_sorts_chronologically_and_says_what_it_was() {
        let a = log_name(1_700_000_000_000, "my-proj", true);
        let b = log_name(1_700_000_001_000, "my-proj", false);
        assert!(a < b, "{a} !< {b}");
        assert!(a.ends_with("-ok.log"), "{a}");
        assert!(b.ends_with("-failed.log"), "{b}");
    }

    /// A project name is a config key an engineer chose; it reaches a
    /// filename here, so anything that is not safe in one is replaced rather
    /// than trusted.
    #[test]
    fn a_project_name_cannot_escape_the_log_directory() {
        let name = log_name(1, "../../etc/passwd", true);
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
    }

    #[test]
    fn a_log_id_from_a_url_cannot_reach_outside_the_directory() {
        for bad in ["", "x", "../secret.log", "a/b.log", "..\\b.log", "no-extension"] {
            assert!(read_log(bad).is_err(), "{bad} was accepted");
        }
    }

    /// The listing's fields come back out of the name it was written under,
    /// so the two have to agree.
    #[test]
    fn a_written_name_parses_back_into_its_own_listing_fields() {
        let id = log_name(1_700_000_000_123, "nrf-dut", false);
        let stem = id.trim_end_matches(".log");
        let mut parts = stem.splitn(2, '-');
        assert_eq!(parts.next().unwrap().parse::<u64>().unwrap(), 1_700_000_000_123);
        let rest = parts.next().unwrap();
        let (project, status) = rest.rsplit_once('-').unwrap();
        assert_eq!(project, "nrf-dut");
        assert_eq!(status, "failed");
    }
}

// ---- build runs: one in-flight build+flash, watchable over SSE -------------

/// One build-and-flash in progress, and everything it has said so far.
///
/// **Backlog plus a broadcast, the same shape `live_study::LiveSession`
/// uses**, and for the same reason: a browser that connects a moment after
/// the run started must see the lines it missed, not join mid-compile. The
/// backlog is bounded because a Zephyr build's output is thousands of lines
/// and the card that renders it is a scroller, not an archive — the whole
/// log is on disk (see [`read_log`]) and the card links to it.
pub struct BuildRun {
    pub id: String,
    tx: tokio::sync::broadcast::Sender<String>,
    backlog: std::sync::Mutex<Vec<String>>,
    finished: std::sync::atomic::AtomicBool,
}

/// How many lines the live card replays to a browser that connects late.
const BACKLOG_LINES: usize = 2000;

impl BuildRun {
    fn new(id: String) -> Arc<BuildRun> {
        let (tx, _) = tokio::sync::broadcast::channel(512);
        Arc::new(BuildRun {
            id,
            tx,
            backlog: std::sync::Mutex::new(Vec::new()),
            finished: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn emit(&self, event: serde_json::Value) {
        let payload = event.to_string();
        if let Ok(mut backlog) = self.backlog.lock() {
            backlog.push(payload.clone());
            if backlog.len() > BACKLOG_LINES {
                let drop = backlog.len() - BACKLOG_LINES;
                backlog.drain(..drop);
            }
        }
        // An error means nobody is subscribed, which is the normal state of
        // a build started from a tab that then navigated away.
        let _ = self.tx.send(payload);
    }

    pub fn finish(&self, event: serde_json::Value) {
        self.emit(event);
        self.finished.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn backlog(&self) -> Vec<String> {
        self.backlog.lock().map(|b| b.clone()).unwrap_or_default()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

/// Every build this process has run, newest last.
#[derive(Default)]
pub struct BuildRuns {
    runs: std::sync::Mutex<Vec<Arc<BuildRun>>>,
}

/// How many finished runs are kept addressable. Small: a finished build's
/// durable record is its log file, and this list exists only so a browser
/// can still fetch the tail of one it was watching.
const MAX_RUNS: usize = 8;

impl BuildRuns {
    pub fn new() -> Arc<BuildRuns> {
        Arc::new(BuildRuns::default())
    }

    pub fn start(&self) -> Arc<BuildRun> {
        let run = BuildRun::new(format!("build-{}", now_utc_ms()));
        let mut runs = self.runs.lock().unwrap();
        runs.push(run.clone());
        while runs.len() > MAX_RUNS {
            match runs.iter().position(|r| r.is_finished()) {
                Some(i) => {
                    runs.remove(i);
                }
                // Never evict one still running: its task is still writing
                // to it.
                None => break,
            }
        }
        run
    }

    pub fn get(&self, id: &str) -> Option<Arc<BuildRun>> {
        self.runs.lock().unwrap().iter().find(|r| r.id == id).cloned()
    }

    pub fn latest(&self) -> Option<Arc<BuildRun>> {
        self.runs.lock().unwrap().last().cloned()
    }
}

// ---- routes ---------------------------------------------------------------

/// `GET /api/build/survey` — what the Build card can offer for the open
/// firmware repo.
pub async fn api_build_survey(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> axum::response::Json<serde_json::Value> {
    let Some(repo) = state.study_designer.repo_path() else {
        return axum::response::Json(
            serde_json::to_value(unavailable(
                "no firmware repo is open, so there is no project to build. Open one first.",
                None,
            ))
            .unwrap_or_default(),
        );
    };
    let configured = state.build_config_path.clone();
    // A survey scans a west workspace off disk, which is filesystem work,
    // not async work.
    let survey = tokio::task::spawn_blocking(move || survey(configured.as_deref(), &repo))
        .await
        .unwrap_or_else(|e| unavailable(format!("the target scan panicked: {e:?}"), None));
    axum::response::Json(serde_json::to_value(survey).unwrap_or_default())
}

#[derive(Debug, serde::Deserialize)]
pub struct BuildEventsQuery {
    #[serde(default)]
    pub id: Option<String>,
}

/// `GET /api/build/events` — one build's lines, as they arrive.
///
/// Same shape as the Live Study tab's own stream: the backlog is replayed
/// first so a browser that connects mid-build sees what it missed, then the
/// broadcast takes over. With no `id` it follows the most recent build,
/// which is what lets the card survive a reload.
pub async fn api_build_events(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    axum::extract::Query(q): axum::extract::Query<BuildEventsQuery>,
) -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse as _;

    let run = match q.id.as_deref().filter(|s| !s.is_empty()) {
        Some(id) => state.build_runs.get(id),
        None => state.build_runs.latest(),
    };
    let Some(run) = run else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "no firmware build has been started in this embarch-ui process yet",
        )
            .into_response();
    };

    let backlog = run.backlog();
    let rx = run.subscribe();
    let stream = futures_util::stream::unfold(
        (rx, backlog.into_iter()),
        |(mut rx, mut pending)| async move {
            if let Some(first) = pending.next() {
                let event = Event::default().event("build").data(first);
                return Some((Ok::<Event, std::convert::Infallible>(event), (rx, pending)));
            }
            match rx.recv().await {
                Ok(payload) => {
                    Some((Ok(Event::default().event("build").data(payload)), (rx, pending)))
                }
                // This browser fell behind our own broadcast. The whole log
                // is on disk, so the frame says which id to go read rather
                // than pretending the card is complete.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => Some((
                    Ok(Event::default().event("build").data(
                        serde_json::json!({ "kind": "lagged", "missed": missed }).to_string(),
                    )),
                    (rx, pending),
                )),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// `GET /api/build/logs` — every stored build log, newest first.
pub async fn api_build_logs() -> axum::response::Json<serde_json::Value> {
    axum::response::Json(serde_json::json!({ "logs": list_logs() }))
}

/// `GET /api/build/logs/{id}` — one stored build log, as text.
pub async fn api_build_log(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    match read_log(&id) {
        Ok(text) => text.into_response(),
        Err(e) => (axum::http::StatusCode::NOT_FOUND, format!("{e:#}")).into_response(),
    }
}
