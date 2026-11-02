//! Making a bench live (bench editor phase 4): **applying a saved bench to
//! Core, and this UI's record of which bench and config are on the desk**.
//!
//! # What goes to Core, and what never does
//!
//! A bench's **declarations** apply straight away, each with its own outcome
//! (a refusal is one row with Core's words, and the rest still apply): every
//! signal, each role's board type, an instrument's line on an `scpi`
//! binding, the DUT's bootload pair, dev-bench's link. Its **enrolments**
//! come back as proposals, because a file is not evidence about what is
//! plugged in (decision 44) — the same split, and for the two roles the same
//! code, as `POST /api/topology/profiles/{slug}/apply` (decision 75).
//!
//! **Nothing is retracted by making a bench live** (owner, 2026-11-01).
//! Core's store is machine-wide, not this repo's, so what Core holds that the
//! bench does not name comes back as **leftovers**, each retracted only by
//! its own `POST /api/bench/live/retract` — and that route refuses anything
//! the leftovers do not list. **What the bench names is read from its raw
//! bindings**, never only from what its templates let it declare: a part
//! whose template is missing declares nothing, and Core's record for it must
//! not then turn into a leftover with a Retract beside it.
//!
//! # The live record: one live bench per machine
//!
//! **Which bench and config are live is this UI's record, not the repo's**
//! (owner): a checkout shared between two machines would otherwise claim one
//! machine's bench on the other. **One bench is live per machine** (owner,
//! 2026-11-01): the last explicit make-live wins machine-wide. The record
//! (`live-benches.json` beside the UI's per-user state, never in a firmware
//! repo) keeps each repo's last live bench, keyed by the repo's canonical
//! path, and one machine-wide entry naming the repo that is live now. A repo
//! another repo took over from reads `superseded`, and its saves do not write
//! through until an explicit make-live takes the machine back. A bench file
//! renamed or deleted on disk leaves the record pointing at nothing, and
//! `GET` says so (`missing: true`) rather than guessing another bench.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use embarch_core_client::{EnrolledBoardResponse, Instrument, InstrumentProtocol, ProbeInfo, UsbPortId};
use serde::{Deserialize, Serialize};

use super::check::Templates;
use super::files::{self, Bench, Binding, Config, LoadError, Part};
use super::routes::{path_slug, refuse, refuse_as, OpenRepo};
use super::{catalog, Catalog, Function, Kind, Role};
use crate::topology::{
    apply_link, apply_role, declare_signals, lookup_chip, no_chip_sentence, now_utc_ms, pick_chip, profile_path, recorded_chip, role_chip,
    write_role_board, BoardWrite, LinkOutcome, RoleBinding,
};
use crate::AppState;

// ---- the live record --------------------------------------------------------

/// Which repo took over the machine from this one, and with what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Superseded {
    pub repo: String,
    pub slug: String,
    pub config: Option<String>,
}

/// One repo's last live bench.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRecord {
    pub slug: String,
    pub config: Option<String>,
    /// When this bench and config became live; a re-apply of the same pair
    /// keeps it.
    pub since_utc_ms: u64,
    /// Set when another repo's explicit make-live took the machine: this
    /// repo's bench is no longer live, and its saves do not write through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Superseded>,
}

/// The machine-wide entry: the one bench live on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineLive {
    pub repo: String,
    pub slug: String,
    pub config: Option<String>,
    pub since_utc_ms: u64,
}

/// The whole record file. **Strict**: a file in an older shape (the first
/// phase-4 build kept a bare map of repos) does not parse, and so is never
/// read as empty and overwritten.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Records {
    #[serde(default)]
    pub repos: BTreeMap<String, LiveRecord>,
    #[serde(default)]
    pub machine: Option<MachineLive>,
}

/// The record as the routes serve it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveView {
    pub slug: String,
    pub config: Option<String>,
    pub since_utc_ms: u64,
    /// The bench file is not there any more: renamed or deleted on disk.
    pub missing: bool,
    /// The bench is there, and the config the record names is not in it
    /// any more (renamed or removed): nothing can be applied or retracted
    /// against it until a config is made live again.
    pub config_missing: bool,
    /// The bench file is there and does not parse: nothing can be applied
    /// or retracted until it is fixed by hand.
    pub unparseable: bool,
}

/// `live-benches.json` beside the recent-projects list: in the directory of
/// the file `EMBARCH_UI_STATE` names when it is set (so a drive that isolates
/// its state isolates this too), else in the per-user data directory the
/// suite already keeps process-written state in. **Never in the firmware
/// repo.**
pub fn default_record_path() -> Option<PathBuf> {
    record_path_from(std::env::var_os("EMBARCH_UI_STATE"), || {
        embarch_core_client::user_dirs::user_data_dir().map_err(|e| format!("{e:#}"))
    })
}

/// [`default_record_path`]'s rule, with the environment passed in so a test
/// can check it without setting a process-wide variable.
fn record_path_from(state: Option<OsString>, data_dir: impl FnOnce() -> Result<PathBuf, String>) -> Option<PathBuf> {
    if let Some(explicit) = state {
        let dir = Path::new(&explicit).parent().map(Path::to_path_buf).unwrap_or_default();
        return Some(dir.join("live-benches.json"));
    }
    data_dir()
        .map(|dir| dir.join("ui").join("live-benches.json"))
        .map_err(|e| tracing::warn!("no per-user data dir for the live-bench record: {e}"))
        .ok()
}

/// Held across every read-modify-write of the record file.
static RECORD: Mutex<()> = Mutex::new(());

/// A repo's key in the record: its canonical path, so two spellings of one
/// checkout are one entry.
fn repo_key(repo: &Path) -> String {
    repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf()).to_string_lossy().into_owned()
}

/// The record file. A missing file is no records; **an unreadable or
/// unparseable one is an error**, never an empty record that a write would
/// then replace.
fn read_records(path: &Path) -> Result<Records, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("the live-bench record {} does not parse: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Records::default()),
        Err(e) => Err(format!("the live-bench record {} could not be read: {e}", path.display())),
    }
}

fn write_records(path: &Path, records: &Records) -> Result<(), String> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(records).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".live-benches.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not write {}: {e}", path.display())
    })
}

/// Runs `change` over the record under the lock and writes it back.
fn update_records<T>(path: &Path, change: impl FnOnce(&mut Records) -> T) -> Result<T, String> {
    let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    let mut records = read_records(path)?;
    let out = change(&mut records);
    write_records(path, &records)?;
    Ok(out)
}

/// The open repo's entry, if any.
pub fn record_for(path: &Path, repo: &Path) -> Result<Option<LiveRecord>, String> {
    let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    Ok(read_records(path)?.repos.remove(&repo_key(repo)))
}

/// An explicit make-live: this repo's entry, and **the machine**. A repo the
/// machine pointed at before is marked superseded by this one. A re-apply of
/// the same bench and config keeps its `since`.
pub fn take_machine(path: &Path, repo: &Path, slug: &str, config: Option<&str>) -> Result<LiveRecord, String> {
    let key = repo_key(repo);
    update_records(path, |r| {
        let since = match r.repos.get(&key) {
            Some(p) if p.slug == slug && p.config.as_deref() == config && p.superseded_by.is_none() => p.since_utc_ms,
            _ => now_utc_ms(),
        };
        if let Some(previous) = r.machine.take().filter(|m| m.repo != key) {
            if let Some(entry) = r.repos.get_mut(&previous.repo) {
                entry.superseded_by = Some(Superseded { repo: key.clone(), slug: slug.to_string(), config: config.map(str::to_string) });
            }
        }
        let record = LiveRecord { slug: slug.to_string(), config: config.map(str::to_string), since_utc_ms: since, superseded_by: None };
        r.repos.insert(key.clone(), record.clone());
        r.machine = Some(MachineLive { repo: key, slug: slug.to_string(), config: config.map(str::to_string), since_utc_ms: since });
        record
    })
}

/// The config this repo's live bench is wired as, **and nothing else**: the
/// entry's config (its `since` moved only when the config changes), the
/// machine's when it points at this repo's bench, and the mark a takeover
/// left on another repo's entry. Refused when the entry is gone or names
/// another bench by the time the record is written.
pub fn set_live_config(path: &Path, repo: &Path, slug: &str, config: Option<&str>) -> Result<LiveRecord, String> {
    let key = repo_key(repo);
    update_records(path, |r| {
        let entry = r.repos.get_mut(&key).filter(|e| e.slug == slug).ok_or_else(|| format!("`{slug}` is no longer this repo's live bench"))?;
        if entry.config.as_deref() != config {
            entry.config = config.map(str::to_string);
            entry.since_utc_ms = now_utc_ms();
        }
        let record = entry.clone();
        if let Some(m) = r.machine.as_mut().filter(|m| m.repo == key && m.slug == slug) {
            m.config = record.config.clone();
            m.since_utc_ms = record.since_utc_ms;
        }
        for other in r.repos.values_mut() {
            if let Some(mark) = other.superseded_by.as_mut().filter(|s| s.repo == key && s.slug == slug) {
                mark.config = record.config.clone();
            }
        }
        Ok(record)
    })?
}

/// Clears this repo's entry, and the machine's **only when it points here**.
pub fn clear_record(path: &Path, repo: &Path) -> Result<(), String> {
    let key = repo_key(repo);
    {
        let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        let records = read_records(path)?;
        if !records.repos.contains_key(&key) && records.machine.as_ref().is_none_or(|m| m.repo != key) {
            return Ok(());
        }
    }
    update_records(path, |r| {
        r.repos.remove(&key);
        if r.machine.as_ref().is_some_and(|m| m.repo == key) {
            r.machine = None;
        }
    })
}

/// Who took the machine from this repo's entry, if anyone: the mark a
/// takeover left, or a machine entry pointing elsewhere.
fn superseded(records: &Records, key: &str) -> Option<Superseded> {
    let entry = records.repos.get(key)?;
    match records.machine.as_ref() {
        Some(m) if m.repo != key => Some(Superseded { repo: m.repo.clone(), slug: m.slug.clone(), config: m.config.clone() }),
        _ => entry.superseded_by.clone(),
    }
}

/// A record as `GET` serves it. **An I/O error telling whether the file is
/// there is reported**, never read as `missing`.
fn view(repo: &Path, record: &LiveRecord) -> Result<LiveView, String> {
    let file = profile_path(repo, &record.slug);
    let present = file
        .try_exists()
        .map_err(|e| format!("could not tell whether {} is there: {e}", files::bench_file(&record.slug)))?;
    let loaded = if present { Some(files::load(repo, &record.slug)) } else { None };
    if let Some(Err(LoadError::Unreadable(e))) = &loaded {
        return Err(e.clone());
    }
    let unparseable = matches!(loaded, Some(Err(LoadError::Unparseable(_))));
    let config_missing = record
        .config
        .as_deref()
        .is_some_and(|name| matches!(&loaded, Some(Ok(b)) if b.config(name).is_none()));
    Ok(LiveView {
        slug: record.slug.clone(),
        config: record.config.clone(),
        since_utc_ms: record.since_utc_ms,
        missing: !present,
        config_missing,
        unparseable,
    })
}

fn record_path(state: &AppState) -> Result<&Path, Box<Response>> {
    state.bench_live_path.as_deref().ok_or_else(|| {
        Box::new(refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "this UI has no per-user data directory to keep the live-bench record in; set EMBARCH_UI_STATE",
        ))
    })
}

fn superseded_refusal(s: &Superseded, doing: &str) -> Box<Response> {
    Box::new(refuse_as(
        StatusCode::CONFLICT,
        "superseded",
        format!(
            "this repo's bench is not live: {} went live later with `{}`{}, and one bench is live per machine — {doing}",
            s.repo,
            s.slug,
            s.config.as_deref().map(|c| format!(" · {c}")).unwrap_or_default()
        ),
    ))
}

// ---- what a report carries --------------------------------------------------

/// One declaration's outcome. `route` is the Core route it went to, or
/// `(not sent)` for a row about what was not declared and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub what: String,
    pub route: String,
    pub ok: bool,
    pub text: String,
}

const NOT_SENT: &str = "(not sent)";

fn row(what: impl Into<String>, route: impl Into<String>, ok: bool, text: impl Into<String>) -> Row {
    Row { what: what.into(), route: route.into(), ok, text: text.into() }
}

/// An enrolment for a human to confirm through `POST /api/enroll`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proposal {
    pub kind: &'static str,
    /// `None` for a bench board.
    pub role: Option<String>,
    /// What `/api/enroll` sends as the board's name: **a role's board type**
    /// (decision 45: a role's name is its board type, which the board write
    /// just set) or **a bench board's label** (`embarch-core` decision 83).
    pub name: String,
    pub chip: String,
    pub probe_serial: String,
    pub part: String,
    pub text: String,
}

/// Why a part with a probe, or a role, gets no proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotProposed {
    pub part: String,
    pub text: String,
}

/// Something Core holds that the live bench does not name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Leftover {
    pub kind: &'static str,
    pub id: String,
    pub text: String,
}

// ---- what a bench names -----------------------------------------------------

/// The configs whose cables are on the desk: the live one alone, or none —
/// with no config live, only the bench-level cables are, so an SWD lead in a
/// config reaches nothing.
fn live_configs(config: Option<&Config>) -> &[Config] {
    config.map(std::slice::from_ref).unwrap_or_default()
}

/// The name an instrument part is declared under: its label, else its id.
fn instrument_name(part: &Part) -> String {
    part.label.as_deref().map(str::trim).filter(|l| !l.is_empty()).unwrap_or(&part.id).to_string()
}

/// A role-less board that reaches a probe is a bench board (`embarch-topology`
/// decision 41); a probe pod, an analyzer or an instrument is not, even with
/// a probe bound.
fn is_bench_board(part: &Part, templates: &Templates) -> bool {
    part.role.is_none() && templates.get(&part.template).is_some_and(|t| t.category.is_bench_board())
}

/// Each binding of `function` on a connector the part's template says
/// carries it, with the connector.
fn bound<'p>(part: &'p Part, templates: &Templates, function: Function) -> Vec<(&'p str, &'p Binding)> {
    let template = templates.get(&part.template);
    part.usb
        .iter()
        .filter(|(cid, _)| template.and_then(|t| t.connector(cid)).is_some_and(|c| c.kind == Kind::UsbHost && c.carries(function)))
        .filter_map(|(cid, functions)| functions.get(&function).map(|b| (cid.as_str(), b)))
        .collect()
}

/// Whether the part's raw `usb` map binds `function` anywhere, whatever its
/// template says — what the bench *names*, as opposed to what it declared.
fn names_function(part: &Part, function: Function) -> bool {
    part.usb.values().any(|f| f.contains_key(&function))
}

/// The first part holding each role: the one applied, and the one named.
fn role_holders(bench: &Bench) -> BTreeMap<Role, &Part> {
    let mut out = BTreeMap::new();
    for p in &bench.parts {
        if let Some(r) = p.role {
            out.entry(r).or_insert(p);
        }
    }
    out
}

/// What a role's part names, for the leftovers and the differences.
struct RoleNamed {
    probe: Option<String>,
    /// Every probe serial the role's part reaches: its own, and the far
    /// end's of each SWD lead from it, bench-level or in the live config.
    reachable: BTreeSet<String>,
    /// **What protects a correct enrolment from a Retract**: a cable from the
    /// role's part whose kind cannot be told — the holder's or the far end's
    /// template missing, or a connector its template does not declare — so
    /// whether it is an SWD lead to the probe Core holds cannot be told, and
    /// no probe-mismatch leftover is offered.
    probe_unknown: bool,
    board: Option<String>,
    link: (Option<String>, Option<u8>),
}

/// The probe serials a part reaches — its own, and the far end's of each SWD
/// lead from it (bench-level or in the live config), followed as
/// `files::reached_probe` and the probe-path check follow one: both
/// connectors `swd`. A cable of another known kind (a radio link, a power
/// lead) reaches no probe. **A cable whose kind cannot be told** — either
/// part's template missing, or a connector its template does not declare —
/// makes the role's probe unknown, so its Retract is not offered for an
/// enrolment that lead may well be.
fn raw_reach(part: &Part, bench: &Bench, configs: &[Config], templates: &Templates) -> (BTreeSet<String>, bool) {
    let mut serials: BTreeSet<String> = part.probe_serial().map(str::to_string).into_iter().collect();
    let mut unknown = !templates.contains_key(&part.template);
    let ends = bench
        .cables
        .iter()
        .map(|c| (c.from.as_str(), c.to.as_str()))
        .chain(configs.iter().flat_map(|c| c.cables.iter().map(|[a, b]| (a.as_str(), b.as_str()))));
    // `Some(kind)` for a connector its template declares, `None` when that
    // cannot be told; a hub (this machine) has no connectors and is skipped.
    enum End<'p> {
        Known(&'p Part, Kind),
        Hub,
        Unknown,
    }
    let resolve = |end: &str| -> End {
        let (pid, cid) = match end.split_once('.') {
            Some((p, c)) => (p, Some(c)),
            None => (end, None),
        };
        let Some(p) = bench.parts.iter().find(|x| x.id == pid) else { return End::Unknown };
        let Some(t) = templates.get(&p.template) else { return End::Unknown };
        if t.hub {
            return End::Hub;
        }
        match cid.and_then(|c| t.connector(c)) {
            Some(c) => End::Known(p, c.kind),
            None => End::Unknown,
        }
    };
    let part_of = |end: &str| end.split('.').next().unwrap_or(end).to_string();
    for (a, b) in ends {
        let (near, far) = if part_of(a) == part.id {
            (a, b)
        } else if part_of(b) == part.id {
            (b, a)
        } else {
            continue;
        };
        match (resolve(near), resolve(far)) {
            (_, End::Hub) => {}
            (End::Known(_, Kind::Swd), End::Known(other, Kind::Swd)) => serials.extend(other.probe_serial().map(str::to_string)),
            (End::Known(..), End::Known(..)) => {}
            _ => unknown = true,
        }
    }
    (serials, unknown)
}

/// What the live bench names, for the leftovers. **From its raw bindings**,
/// so a missing or broken template never turns Core's record into a
/// leftover: any `scpi` key names an instrument, a `bootload-bootloader` key
/// on the DUT names a pair, any role-less part with a probe names a bench
/// board.
struct Named {
    signals: BTreeSet<String>,
    instruments: BTreeSet<String>,
    roles: BTreeMap<String, RoleNamed>,
    bench_boards: BTreeSet<String>,
    bootload: bool,
}

fn named(bench: &Bench, templates: &Templates, config: Option<&Config>) -> Named {
    let configs = live_configs(config);
    let reach = |p: &Part| {
        files::reached_probe(p, &bench.parts, &bench.cables, configs, templates)
            .ok()
            .flatten()
            .or_else(|| p.probe_serial().map(str::to_string))
    };
    let holders = role_holders(bench);
    let mut bench_boards: BTreeSet<String> = bench.parts.iter().filter(|p| p.role.is_none()).filter_map(|p| p.probe_serial().map(str::to_string)).collect();
    bench_boards.extend(bench.parts.iter().filter(|p| is_bench_board(p, templates)).filter_map(reach));
    Named {
        signals: bench.signals.iter().map(|s| s.name.clone()).collect(),
        instruments: bench.parts.iter().filter(|p| names_function(p, Function::Scpi)).map(instrument_name).collect(),
        roles: holders
            .iter()
            .map(|(r, p)| {
                let binding = files::role_binding_in(&bench.parts, &bench.cables, configs, *r, templates);
                let probe = binding.as_ref().map(|b| b.probe_serial.clone()).filter(|s| !s.is_empty()).or_else(|| reach(p));
                let link = binding.map(|b| (b.link_port_serial, b.link_port_interface)).unwrap_or_default();
                let (mut reachable, probe_unknown) = raw_reach(p, bench, configs, templates);
                reachable.extend(probe.clone());
                (r.as_str().to_string(), RoleNamed { probe, reachable, probe_unknown, board: p.board.clone().filter(|b| !b.trim().is_empty()), link })
            })
            .collect(),
        bench_boards,
        bootload: holders.get(&Role::Dut).is_some_and(|p| names_function(p, Function::BootloadBootloader)),
    }
}

/// What a leftovers pass found: the leftovers, a row for each Core read that
/// did not answer (keyed by the leftover kinds it covers), and the rows where
/// Core's record for something the bench names differs from the bench.
#[derive(Default)]
struct LeftoverPass {
    leftovers: Vec<Leftover>,
    unread: Vec<(&'static [&'static str], Row)>,
    differs: Vec<Row>,
}

impl LeftoverPass {
    /// Why a kind could not be compared, when its Core read failed.
    fn unread_for(&self, kind: &str) -> Option<&Row> {
        self.unread.iter().find(|(kinds, _)| kinds.contains(&kind)).map(|(_, r)| r)
    }

    fn unread_rows(&self) -> Vec<Row> {
        self.unread.iter().map(|(_, r)| r.clone()).collect()
    }
}

fn link_text(serial: &Option<String>, interface: Option<u8>) -> String {
    match (serial, interface) {
        (None, None) => "none".to_string(),
        (Some(s), None) => format!("serial {s}"),
        (None, Some(i)) => format!("interface {i}"),
        (Some(s), Some(i)) => format!("serial {s}, interface {i}"),
    }
}

/// What Core holds that the bench does not name, and a row for each Core
/// read that did not answer — **an unanswered read is not "no leftovers"**.
async fn leftovers(state: &AppState, named: &Named) -> LeftoverPass {
    let mut pass = LeftoverPass::default();
    let not_read = |route: &str, what: &str, e: anyhow::Error| {
        row(format!("leftover {what}"), route, false, format!("Core did not answer {route} ({e:#}), so its {what} were not compared with this bench"))
    };

    match state.core.list_signals().await {
        Ok(signals) => pass.leftovers.extend(signals.into_iter().filter(|s| !named.signals.contains(&s.name)).map(|s| Leftover {
            kind: "signal",
            text: format!("Core declares the signal `{}`, which this bench does not name.", s.name),
            id: s.name,
        })),
        Err(e) => pass.unread.push((&["signal"], not_read("GET /signals", "signals", e))),
    }
    match state.core.instruments().await {
        Ok(instruments) => pass.leftovers.extend(instruments.into_iter().filter(|i| !named.instruments.contains(&i.name)).map(|i| Leftover {
            kind: "instrument",
            text: format!("Core declares the instrument `{}`, which this bench does not name.", i.name),
            id: i.name,
        })),
        Err(e) => pass.unread.push((&["instrument"], not_read("GET /instruments", "instruments", e))),
    }
    match state.core.list_enrolled().await {
        Ok(rows) => {
            for r in rows {
                let probe = r.probe_serial.clone();
                if r.role.is_empty() {
                    let Some(serial) = probe else { continue };
                    if !named.bench_boards.contains(&serial) {
                        pass.leftovers.push(Leftover {
                            kind: "bench-board",
                            text: format!("Core enrols the bench board `{}` on probe {serial}, which this bench does not have.", r.name),
                            id: serial,
                        });
                    }
                    continue;
                }
                let on = probe.as_deref().map(|s| format!("probe {s}")).unwrap_or_else(|| "no probe".to_string());
                let Some(bench_role) = named.roles.get(&r.role) else {
                    pass.leftovers.push(Leftover {
                        kind: "role",
                        text: format!("Core holds the {} role ({}, {on}); this bench has no part in that role.", r.role, if r.name.is_empty() { "no board type" } else { &r.name }),
                        id: r.role,
                    });
                    continue;
                };
                // Offered only when Core's probe is none the role's part
                // reaches over a raw cable, and only when every template on
                // the way is known.
                if probe.as_ref().is_some_and(|s| !bench_role.reachable.contains(s)) && !bench_role.probe_unknown {
                    pass.leftovers.push(Leftover {
                        kind: "role",
                        text: format!(
                            "Core enrols {on} in the {} role; this bench binds {} there.",
                            r.role,
                            bench_role.probe.as_deref().map(|s| format!("probe {s}")).unwrap_or_else(|| "no probe".to_string())
                        ),
                        id: r.role.clone(),
                    });
                }
                if let Some(board) = &bench_role.board {
                    if r.name != *board {
                        pass.differs.push(row(
                            format!("{} board (Core)", r.role),
                            "GET /probes/enrolled",
                            false,
                            format!(
                                "Core's {} role holds the board type `{}`; this bench names `{board}`",
                                r.role,
                                if r.name.is_empty() { "(none)" } else { &r.name }
                            ),
                        ));
                    }
                }
                if r.role == "dev-bench" {
                    let core_link = (r.link_port_serial.clone(), r.link_port_interface);
                    if core_link != bench_role.link {
                        pass.differs.push(row(
                            "dev-bench link (Core)",
                            "GET /probes/enrolled",
                            false,
                            format!(
                                "Core's dev-bench link is {}; this bench binds {}",
                                link_text(&core_link.0, core_link.1),
                                link_text(&bench_role.link.0, bench_role.link.1)
                            ),
                        ));
                    }
                }
            }
        }
        Err(e) => pass.unread.push((&["bench-board", "role"], not_read("GET /probes/enrolled", "enrolled boards", e))),
    }
    match state.core.bootload_ports().await {
        Ok(Some(pair)) if !named.bootload => {
            let role = if pair.role.is_empty() { "dut".to_string() } else { pair.role.clone() };
            pass.leftovers.push(Leftover {
                kind: "bootload",
                text: format!("Core holds a bootload pair for {role} (bootloader {}), which this bench does not declare.", port_text(&pair.bootloader)),
                id: role,
            });
        }
        Ok(_) => {}
        Err(e) => pass.unread.push((&["bootload"], not_read("GET /bootload/ports", "bootload ports", e))),
    }
    pass
}

fn port_text(p: &UsbPortId) -> String {
    let mut s = format!("{:04x}:{:04x}", p.vid, p.pid);
    if let Some(serial) = &p.serial {
        s.push_str(&format!(" serial {serial}"));
    }
    if let Some(i) = p.interface {
        s.push_str(&format!(" interface {i}"));
    }
    s
}

/// A binding as the USB identity Core finds a port by, or why it is not one:
/// Core matches a port on its VID:PID (`embarch-topology` decision 36), so a
/// binding that names neither cannot be declared.
fn port_of(binding: &Binding, what: &str) -> Result<UsbPortId, String> {
    match binding {
        Binding::Port(p) => match (p.vid, p.pid) {
            (Some(vid), Some(pid)) => Ok(UsbPortId { vid, pid, serial: p.serial.clone(), interface: p.interface }),
            _ => Err(format!("{what} names no VID and PID, which Core finds a port by — bind it from Core's port list")),
        },
        Binding::Serial(_) | Binding::Interface(_) => {
            Err(format!("{what} names no VID and PID, which Core finds a port by — bind it from Core's port list"))
        }
    }
}

/// The facts a proposal carries, as words after its sentence. `moved` is
/// what confirming it would move — said only when Core's row holds a probe
/// that is not this one.
fn facts(already: bool, attached: bool, moved: Option<String>, already_text: &str) -> String {
    let mut s = String::new();
    if already {
        s.push_str(&format!("; {already_text}"));
    }
    if !attached {
        s.push_str("; the probe is not attached now, so confirming fails until it is");
    }
    if let (false, Some(m)) = (already, moved) {
        s.push_str(&format!("; {m}"));
    }
    s
}

// ---- applying ---------------------------------------------------------------

/// Everything making a bench live reports.
#[derive(Debug, Default, Serialize)]
pub struct Applied {
    pub applied: Vec<Row>,
    pub proposals: Vec<Proposal>,
    pub not_proposed: Vec<NotProposed>,
}

/// The rows for what a bench could not declare because its templates do not
/// say how: each template file that would not load, each part whose template
/// is missing, each binding on a connector that does not carry it.
fn template_rows(bench: &Bench, catalog: &Catalog) -> Vec<Row> {
    let mut out: Vec<Row> = catalog
        .errors
        .iter()
        .map(|e| row(format!("part template {}", e.file), NOT_SENT, false, format!("{}: {} — a part drawn from it declares nothing", e.file, e.error)))
        .collect();
    let templates = catalog.by_id();
    let dev_bench_holder = role_holders(bench).get(&Role::DevBench).map(|p| p.id.clone());
    for part in &bench.parts {
        let Some(template) = templates.get(&part.template) else {
            out.push(row(
                format!("part {}", part.id),
                NOT_SENT,
                false,
                format!(
                    "{} is drawn from the part template `{}`, which neither the built-ins nor embarch/parts/ has, so nothing it binds was declared or proposed; Core's records its bindings name are not offered as leftovers",
                    part.display(),
                    part.template
                ),
            ));
            continue;
        };
        for (cid, functions) in &part.usb {
            let connector = template.connector(cid).filter(|c| c.kind == Kind::UsbHost);
            for function in functions.keys() {
                let why = match connector {
                    None => format!("`{cid}` is not a usb-host connector of a {}", template.label),
                    Some(c) if !c.carries(*function) => format!("{} · {} does not carry `{}`", part.display(), c.label, function.as_str()),
                    Some(_) => continue,
                };
                // A probe and a VCOM are found by their binding alone, so the
                // role and bench-board steps still use one; say that rather
                // than "not declared".
                let what_happens = match function {
                    Function::Probe => "it is still read as the part's probe".to_string(),
                    Function::Vcom if dev_bench_holder.as_deref() == Some(part.id.as_str()) => "it is still read as dev-bench's VCOM".to_string(),
                    _ => "it was not declared".to_string(),
                };
                out.push(row(format!("binding {} · {cid} · {}", part.display(), function.as_str()), NOT_SENT, false, format!("{why}; {what_happens}")));
            }
        }
    }
    let dut = role_holders(bench).get(&Role::Dut).map(|p| p.id.clone());
    for part in bench.parts.iter().filter(|p| Some(&p.id) != dut.as_ref()) {
        for function in [Function::BootloadApp, Function::BootloadBootloader] {
            if names_function(part, function) {
                out.push(row(
                    format!("binding {} · {}", part.display(), function.as_str()),
                    NOT_SENT,
                    false,
                    format!("{} binds `{}`, but only the DUT's bootload pair is declared, and it does not hold the dut role; not declared", part.display(), function.as_str()),
                ));
            }
        }
    }
    out
}

/// Applies a bench's declarations and decides its proposals. Every Core
/// write goes through `embarch-core-client`; each outcome is its own row.
async fn apply_bench(state: &AppState, bench: &Bench, catalog: &Catalog, config: Option<&Config>) -> Applied {
    let templates = catalog.by_id();
    let templates = &templates;
    let mut out = Applied { applied: template_rows(bench, catalog), ..Applied::default() };
    let configs = live_configs(config);
    let snapshot = state.snapshot_rx.borrow().clone();
    let enrolled = match state.core.list_enrolled().await {
        Ok(rows) => rows,
        Err(e) => {
            out.applied.push(row(
                "enrolled boards",
                "GET /probes/enrolled",
                false,
                format!("Core did not answer ({e:#}); the proposals' facts come from the last poll"),
            ));
            snapshot.enrolled.clone()
        }
    };
    let probes: &[ProbeInfo] = &snapshot.probes;

    for (name, outcome) in declare_signals(state, &bench.signals).await {
        out.applied.push(row(format!("signal {name}"), "POST /signals", outcome.is_ok(), outcome.err().unwrap_or_else(|| "declared".to_string())));
    }

    // ---- the two roles, through the step the topology apply shares ----
    let holders = role_holders(bench);
    for p in &bench.parts {
        if let Some(r) = p.role {
            if holders.get(&r).is_some_and(|first| first.id != p.id) {
                out.not_proposed.push(NotProposed {
                    part: p.id.clone(),
                    text: format!(
                        "{}: also holds the {} role, which {} holds — a role is one slot, so it is not applied or proposed",
                        p.display(),
                        r.as_str(),
                        holders[&r].display()
                    ),
                });
            }
        }
    }
    let mut dev_bench: Option<(RoleBinding, bool)> = None;
    for role in [Role::DevBench, Role::Dut] {
        let Some(part) = holders.get(&role) else { continue };
        if !templates.contains_key(&part.template) {
            continue;
        }
        let Some(binding) = files::role_binding_in(&bench.parts, &bench.cables, configs, role, templates) else { continue };
        let r = role.as_str();
        let outcome = apply_role(state, r, &binding, &enrolled, probes).await;
        if let Some(board) = &outcome.board {
            let route = format!("PUT /probes/enrolled/{r}/board");
            out.applied.push(match board {
                BoardWrite::Applied { chip } => row(format!("{r} board {} ({chip})", binding.board), route, true, "applied"),
                BoardWrite::Refused { chip, error } => row(format!("{r} board {} ({chip})", binding.board), route, false, error.clone()),
                BoardWrite::NoChip(sentence) => row(format!("{r} board {}", binding.board), route, false, sentence.clone()),
            });
        }
        let board_written = outcome.board.as_ref().is_some_and(BoardWrite::ok);
        match outcome.enrol {
            Err(why) => out.not_proposed.push(NotProposed { part: part.id.clone(), text: why }),
            Ok(p) => {
                // A displacement is Core's row holding *another* probe — not
                // the board-only row this apply's own board write made.
                let moved = enrolled
                    .iter()
                    .find(|b| b.role == r)
                    .and_then(|b| b.probe_serial.as_deref().filter(|s| *s != binding.probe_serial).map(|s| (b, s)))
                    .map(|(b, s)| format!("displaces {} on probe {s}", if b.name.is_empty() { "the board" } else { &b.name }));
                let board = if binding.board.is_empty() { String::new() } else { format!(" as {}", binding.board) };
                out.proposals.push(Proposal {
                    kind: "enroll",
                    role: Some(r.to_string()),
                    name: binding.board.clone(),
                    text: format!(
                        "Enrol probe {} in the {r} role{board} ({}){}",
                        binding.probe_serial,
                        p.chip,
                        facts(p.already_enrolled, p.probe_attached, moved, "the role already holds this probe")
                    ),
                    chip: p.chip,
                    probe_serial: binding.probe_serial.clone(),
                    part: part.id.clone(),
                });
            }
        }
        if role == Role::DevBench {
            dev_bench = Some((binding, board_written));
        }
    }

    // ---- bench boards: role-less boards that reach a probe ----
    for part in bench.parts.iter().filter(|p| is_bench_board(p, templates)) {
        let name = part.display().to_string();
        let serial = match files::reached_probe(part, &bench.parts, &bench.cables, configs, templates) {
            Ok(Some(serial)) => serial,
            Ok(None) => continue,
            Err(note) => {
                out.not_proposed.push(NotProposed { part: part.id.clone(), text: format!("{name}: {note} — not proposed") });
                continue;
            }
        };
        let board = part.board.as_deref().map(str::trim).unwrap_or_default();
        let recorded = part.chip.as_ref().map(|c| c.chip.as_str()).unwrap_or_default();
        let recorded_for = part.chip.as_ref().map(|c| c.board.as_str());
        let chip = if board.is_empty() {
            recorded_chip(recorded, recorded_for, "")
                .map(Option::unwrap_or_default)
                .map_err(|note| format!("{name}: not proposed — no board type to look a chip up for, and the {note}"))
        } else {
            pick_chip(lookup_chip(state, "", board).await, recorded, recorded_for, board)
                .map_err(|why| format!("{name}: not proposed — {}", no_chip_sentence(board, &why)))
        };
        let chip = match chip {
            Ok(c) if !c.is_empty() => c,
            Ok(_) => {
                out.not_proposed.push(NotProposed {
                    part: part.id.clone(),
                    text: format!("{name}: no board type and no recorded chip, so nothing to attach the probe as — not proposed"),
                });
                continue;
            }
            Err(text) => {
                out.not_proposed.push(NotProposed { part: part.id.clone(), text });
                continue;
            }
        };
        let already = enrolled.iter().any(|b| b.role.is_empty() && b.probe_serial.as_deref() == Some(serial.as_str()));
        let attached = probes.iter().any(|p| p.serial_number.as_deref() == Some(serial.as_str()));
        let vacated = enrolled
            .iter()
            .find(|b| !b.role.is_empty() && b.probe_serial.as_deref() == Some(serial.as_str()))
            .map(|b: &EnrolledBoardResponse| format!("Core's {} role ({}) holds this probe and would be vacated", b.role, if b.name.is_empty() { "no board type" } else { &b.name }));
        out.proposals.push(Proposal {
            kind: "enroll",
            role: None,
            text: format!("Enrol {name} as a bench board on probe {serial} ({chip}){}", facts(already, attached, vacated, "already enrolled on this probe")),
            name,
            chip,
            probe_serial: serial,
            part: part.id.clone(),
        });
    }

    // ---- instruments: one per part with an `scpi` binding, one per name ----
    let mut instrument_names: BTreeMap<String, String> = BTreeMap::new();
    for part in &bench.parts {
        let lines = bound(part, templates, Function::Scpi);
        let Some((cid, binding)) = lines.first() else { continue };
        let name = instrument_name(part);
        let what = format!("instrument {name}");
        if let Some(first) = instrument_names.get(&name) {
            out.applied.push(row(
                what,
                "PUT /instruments",
                false,
                format!("{} and {first} are both named `{name}`, and Core keeps one instrument per name, so {}'s is not declared", part.id, part.id),
            ));
            continue;
        }
        instrument_names.insert(name.clone(), part.id.clone());
        for (extra, _) in lines.iter().skip(1) {
            out.applied.push(row(
                format!("instrument {name} · {extra}"),
                "PUT /instruments",
                false,
                format!("{} binds a second scpi line on `{extra}`; a part declares one instrument, so it is not declared", part.display()),
            ));
        }
        let facts = templates
            .get(&part.template)
            .and_then(|t| t.connector(cid))
            .and_then(|c| c.functions.iter().find(|f| f.function == Function::Scpi))
            .cloned();
        let port = match port_of(binding, &format!("{} · {cid}'s scpi binding", part.display())) {
            Ok(p) => p,
            Err(why) => {
                out.applied.push(row(what, "PUT /instruments", false, why));
                continue;
            }
        };
        let Some((f, baud)) = facts.and_then(|f| f.baud.map(|b| (f, b))) else {
            out.applied.push(row(what, "PUT /instruments", false, format!("the {} template states no baud for its scpi line, and Core needs one", part.template)));
            continue;
        };
        let inst = Instrument {
            name: name.clone(),
            protocol: InstrumentProtocol::Scpi,
            port,
            baud,
            write_terminator: f.write_terminator.clone().unwrap_or_else(|| "\n".to_string()),
            read_terminator: f.read_terminator.clone().unwrap_or_else(|| "\r\n".to_string()),
            min_interval_ms: f.min_interval_ms.unwrap_or(0),
            ack_ok: f.ack_ok.clone(),
            ack_err: f.ack_err.clone(),
        };
        out.applied.push(match state.core.declare_instrument(&inst).await {
            Ok(stored) => row(what, "PUT /instruments", true, format!("declared on {} at {} baud", port_text(&stored.port), stored.baud)),
            Err(e) => row(what, "PUT /instruments", false, format!("{e:#}")),
        });
    }

    // ---- the DUT's bootload pair ----
    if let Some(dut) = holders.get(&Role::Dut).filter(|p| templates.contains_key(&p.template)) {
        let app = bound(dut, templates, Function::BootloadApp).first().map(|(cid, b)| port_of(b, &format!("{} · {cid}'s bootload-app binding", dut.display())));
        let bootloader = bound(dut, templates, Function::BootloadBootloader)
            .first()
            .map(|(cid, b)| port_of(b, &format!("{} · {cid}'s bootload-bootloader binding", dut.display())));
        let what = "bootload ports";
        match (app, bootloader) {
            (None, None) => {}
            (Some(_), None) => out.applied.push(row(
                what,
                "PUT /bootload/ports",
                false,
                format!("{} binds bootload-app but not bootload-bootloader, and Core needs the bootloader's port; nothing was declared", dut.display()),
            )),
            (Some(Err(why)), Some(_)) | (_, Some(Err(why))) => out.applied.push(row(what, "PUT /bootload/ports", false, why)),
            (app, Some(Ok(bootloader))) => {
                let app = app.and_then(Result::ok);
                out.applied.push(match state.core.declare_bootload_ports(app.as_ref(), &bootloader).await {
                    Ok(_) => row(
                        what,
                        "PUT /bootload/ports",
                        true,
                        format!(
                            "declared: bootloader {}{}",
                            port_text(&bootloader),
                            app.as_ref().map(|a| format!(", app {}", port_text(a))).unwrap_or_default()
                        ),
                    ),
                    Err(e) => row(what, "PUT /bootload/ports", false, format!("{e:#}")),
                });
            }
        }
    }

    // ---- dev-bench's link, decided on the row as it stands after the board
    //      write, and worded after the proposals are decided ----
    if let Some((binding, board_written)) = dev_bench {
        let pre = enrolled.iter().find(|b| b.role == "dev-bench");
        let foreign = pre.and_then(|b| b.probe_serial.as_deref()).filter(|s| *s != binding.probe_serial);
        let has_link = binding.link_port_serial.is_some() || binding.link_port_interface.is_some();
        if let (true, Some(other)) = (has_link, foreign) {
            let proposed = out.proposals.iter().any(|p| p.role.as_deref() == Some("dev-bench"));
            let next = if proposed {
                "confirm this bench's dev-bench enrolment below, and the page applies this bench again"
            } else {
                "this bench proposes no dev-bench enrolment (its row says why); enrol dev-bench, then make this bench live again"
            };
            out.applied.push(row(
                "dev-bench link",
                "POST /dev-bench/link",
                false,
                format!("not declared: Core's dev-bench row holds probe {other}, which this bench does not bind there, and the link would amend that board's row — {next}"),
            ));
        } else {
            let row_exists = pre.is_some() || board_written;
            let proposed = out.proposals.iter().any(|p| p.role.as_deref() == Some("dev-bench"));
            if let Some(link) = apply_link(state, binding.link_port_serial.as_deref(), binding.link_port_interface, row_exists, proposed).await {
                let text = match &link {
                    LinkOutcome::Deferred => "deferred: Core amends the dev-bench row, so the role has to hold a board first — confirm its enrolment below, and the page applies this bench again to declare the link".to_string(),
                    other => other.text("make this bench live again"),
                };
                out.applied.push(row("dev-bench link", "POST /dev-bench/link", link.ok(), text));
            }
        }
    }
    out
}

// ---- routes -----------------------------------------------------------------

/// `GET /api/bench/live` — the open repo's live bench, or `null`, and which
/// repo took the machine from it, if one did.
pub async fn api_live(State(state): State<AppState>, OpenRepo(repo): OpenRepo) -> Response {
    let path = match record_path(&state) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    let records = {
        let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        read_records(path)
    };
    let records = match records {
        Ok(r) => r,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let key = repo_key(&repo);
    let live = match records.repos.get(&key).map(|r| view(&repo, r)).transpose() {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    Json(serde_json::json!({ "live": live, "superseded": superseded(&records, &key) })).into_response()
}

/// `DELETE /api/bench/live` — clears the open repo's entry, and the
/// machine's only when it points here. **Touches nothing in Core.**
pub async fn api_clear_live(State(state): State<AppState>, OpenRepo(repo): OpenRepo) -> Response {
    let _live = state.bench_live_lock.lock().await;
    let path = match record_path(&state) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    match clear_record(path, &repo) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// A key that must be present and may be `null`: `Some(None)` for `null`,
/// `None` (by `default`) for absent.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct LiveConfigRequest {
    /// The config to record as the live wiring, or `null` for the
    /// bench-level cables alone. **Required**.
    #[serde(default, deserialize_with = "present")]
    pub config: Option<Option<String>>,
    /// The bench the caller believes is live: when given, a record naming
    /// another bench is `409 not-live` rather than a config set on it.
    #[serde(default)]
    pub slug: Option<String>,
}

/// `POST /api/bench/live/config` — **records which config the live bench is
/// wired as, and calls nothing in Core** (owner, 2026-11-01: switching
/// configs never calls Core; a config is a named set of part-to-part
/// cables, which declare nothing). Answers the record as `GET` does.
///
/// The config must be in the bench **as saved**. Refused `409`: no record,
/// its file gone, or `slug` naming another bench (`not-live`); another repo
/// holding the machine (`superseded`); the file broken (`unparseable`); the
/// config not in it (`config-missing`). A rename of the live config on the
/// canvas lands here after its save; a prompt naming a config marks it here
/// once it is answered. Under the same lock as a make-live, a retract and a
/// clear, with every check made after taking it. Declaring again is still
/// an explicit make-live or a write-through.
pub async fn api_live_config(State(state): State<AppState>, OpenRepo(repo): OpenRepo, Json(req): Json<LiveConfigRequest>) -> Response {
    let Some(config) = req.config else {
        return refuse(StatusCode::BAD_REQUEST, "a config change names `config`: a config of the live bench, or null for its bench-level cables alone");
    };
    let path = match record_path(&state) {
        Ok(p) => p.to_path_buf(),
        Err(resp) => return *resp,
    };
    let _live = state.bench_live_lock.lock().await;
    let records = {
        let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        read_records(&path)
    };
    let records = match records {
        Ok(r) => r,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let key = repo_key(&repo);
    let Some(record) = records.repos.get(&key) else {
        return refuse_as(StatusCode::CONFLICT, "not-live", "no bench is live in this repo, so no config is recorded as its wiring; make a bench live first");
    };
    if let Some(slug) = req.slug.as_deref().filter(|s| *s != record.slug) {
        return refuse_as(
            StatusCode::CONFLICT,
            "not-live",
            format!("`{slug}` is not this repo's live bench (`{}` is), so its config is not recorded", record.slug),
        );
    }
    if let Some(s) = superseded(&records, &key) {
        return *superseded_refusal(&s, "make it live explicitly to take the machine back, then pick its config");
    }
    let bench = match files::load(&repo, &record.slug) {
        Ok(b) => b,
        Err(LoadError::Missing) => {
            return refuse_as(
                StatusCode::CONFLICT,
                "not-live",
                format!("the live bench `{}` is not on disk any more, so none of its configs can be recorded; make a bench live again", record.slug),
            )
        }
        Err(LoadError::Unparseable(e)) => return refuse_as(StatusCode::CONFLICT, "unparseable", format!("{e} — fix it by hand, then pick its config")),
        Err(LoadError::Unreadable(e)) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if let Some(name) = config.as_deref().filter(|n| bench.config(n).is_none()) {
        return refuse_as(
            StatusCode::CONFLICT,
            "config-missing",
            format!("`{}` as saved has no config named `{name}`; save the bench first, or pick a config it has", record.slug),
        );
    }
    let slug = record.slug.clone();
    match set_live_config(&path, &repo, &slug, config.as_deref()).and_then(|r| view(&repo, &r)) {
        Ok(live) => Json(serde_json::json!({ "live": live, "superseded": null })).into_response(),
        Err(e) => refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Debug, Deserialize)]
pub struct MakeLiveRequest {
    #[serde(default)]
    pub config: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// `true` for the write-through after a Save (or after confirming the
    /// dev-bench enrolment): it re-applies **only the bench already live in
    /// this repo, and only while this repo holds the machine**. `false` — an
    /// explicit make-live — takes the machine. **Required**: a call that
    /// does not say which it is is refused, rather than taking the machine
    /// by default.
    #[serde(default)]
    pub write_through: Option<bool>,
}

/// `POST /api/bench/benches/{slug}/live` — applies the bench **as saved**
/// to Core, records it live, and answers the applied rows, the proposals,
/// the parts not proposed and the leftovers.
///
/// `version` is the one the browser last read, refused `409`
/// (`version-changed`) when the file changed since — the browser saves
/// first — and the bench is read from disk, never from the request. An
/// explicit call takes the machine (one bench live per machine); a
/// `write_through` call is refused `409` (`not-live`) for a bench that is not
/// this repo's live one and (`superseded`) once another repo took the
/// machine, and (`config-differs`) when it names another config than the
/// live one; (`config-missing`) when the live config is gone from the bench.
///
/// **One make-live, retract or clear at a time** (`AppState::bench_live_lock`,
/// held across the whole call, Core included, with every check made after
/// taking it): a write-through checked against the record and then
/// overtaken by an explicit make-live while Core answered would otherwise
/// take the machine back.
pub async fn api_make_live(
    State(state): State<AppState>,
    OpenRepo(repo): OpenRepo,
    UrlPath(slug): UrlPath<String>,
    Json(req): Json<MakeLiveRequest>,
) -> Response {
    let slug = match path_slug(&slug) {
        Ok(s) => s.to_string(),
        Err(resp) => return *resp,
    };
    let Some(write_through) = req.write_through else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "a make-live names `write_through`: false for an explicit make-live, which takes the machine; true for the write-through after a save",
        );
    };
    let path = match record_path(&state) {
        Ok(p) => p.to_path_buf(),
        Err(resp) => return *resp,
    };
    let _live = state.bench_live_lock.lock().await;
    let (file, current) = match files::read_file(&repo, &slug) {
        Ok(read) => read,
        Err(LoadError::Missing) => return refuse(StatusCode::NOT_FOUND, format!("no saved bench `{slug}`")),
        Err(LoadError::Unparseable(e)) => {
            return refuse_as(StatusCode::CONFLICT, "unparseable", format!("{e} — the file does not parse, so it cannot go live; fix it by hand"))
        }
        Err(LoadError::Unreadable(e)) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let Some(version) = req.version.clone() else {
        return refuse(StatusCode::BAD_REQUEST, "making a bench live names the `version` it last read; save first, then send that version");
    };
    if version != current {
        return refuse_as(
            StatusCode::CONFLICT,
            "version-changed",
            format!("{} changed since it was read; reload it, then make it live", files::bench_file(&slug)),
        );
    }
    let records = {
        let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        read_records(&path)
    };
    let records = match records {
        Ok(r) => r,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let key = repo_key(&repo);
    let previous = records.repos.get(&key);
    if write_through {
        let Some(p) = previous.filter(|p| p.slug == slug) else {
            return refuse_as(
                StatusCode::CONFLICT,
                "not-live",
                format!("`{slug}` is not this repo's live bench, so a save does not write it through; make it live explicitly"),
            );
        };
        if let Some(s) = superseded(&records, &key) {
            return *superseded_refusal(&s, "a save writes nothing through; make it live explicitly to take the machine back");
        }
        if p.config != req.config {
            return refuse_as(
                StatusCode::CONFLICT,
                "config-differs",
                format!(
                    "a write-through re-applies the live config ({}), and this one names {}; make that config live explicitly",
                    p.config.as_deref().map(|c| format!("`{c}`")).unwrap_or_else(|| "no config".to_string()),
                    req.config.as_deref().map(|c| format!("`{c}`")).unwrap_or_else(|| "no config".to_string())
                ),
            );
        }
    }
    let bench = files::bench_from_file(&slug, file, Some(current));
    let config = match req.config.as_deref() {
        None => None,
        Some(name) => match bench.config(name) {
            Some(c) => Some(c),
            None if previous.is_some_and(|p| p.slug == slug && p.config.as_deref() == Some(name)) => {
                return refuse_as(
                    StatusCode::CONFLICT,
                    "config-missing",
                    format!("the live config `{name}` is no longer in `{slug}` (renamed or removed); make one of its configs live"),
                )
            }
            None => return refuse(StatusCode::BAD_REQUEST, format!("the bench `{slug}` has no config named `{name}`")),
        },
    };
    let catalog = catalog(Some(&repo));
    let templates = catalog.by_id();
    let mut report = apply_bench(&state, &bench, &catalog, config).await;

    if let Err(e) = take_machine(&path, &repo, &slug, req.config.as_deref()) {
        report.applied.push(row("live record", "(this UI)", false, format!("applied, but the record of which bench is live was not written: {e}")));
    }
    let live = match record_for(&path, &repo) {
        Ok(Some(r)) => match view(&repo, &r) {
            Ok(v) => Some(v),
            Err(e) => {
                report.applied.push(row("live record", "(this UI)", false, e));
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            report.applied.push(row("live record", "(this UI)", false, e));
            None
        }
    };

    let pass = leftovers(&state, &named(&bench, &templates, config)).await;
    report.applied.extend(pass.unread_rows());
    report.applied.extend(pass.differs);
    state.poke.notify_one();
    Json(serde_json::json!({
        "live": live,
        "applied": report.applied,
        "proposals": report.proposals,
        "not_proposed": report.not_proposed,
        "leftovers": pass.leftovers,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RetractRequest {
    pub kind: String,
    pub id: String,
}

/// The live bench, read from its file, and its live config — or the `409`
/// saying there is none to compare Core with: no record (`not-live`), the
/// file gone (`not-live`) or broken (`unparseable`), another repo holding the
/// machine (`superseded`), the config gone (`config-missing`).
fn live_bench(state: &AppState, repo: &Path) -> Result<(Bench, Option<String>), Box<Response>> {
    let path = record_path(state)?;
    let records = {
        let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        read_records(path)
    }
    .map_err(|e| Box::new(refuse(StatusCode::INTERNAL_SERVER_ERROR, e)))?;
    let key = repo_key(repo);
    let Some(record) = records.repos.get(&key) else {
        return Err(Box::new(refuse_as(StatusCode::CONFLICT, "not-live", "no bench is live in this repo, so nothing is a leftover yet; make a bench live first")));
    };
    if let Some(s) = superseded(&records, &key) {
        return Err(superseded_refusal(&s, "leftovers are retracted from the live bench; make this one live to take the machine back"));
    }
    let bench = match files::load(repo, &record.slug) {
        Ok(bench) => bench,
        Err(LoadError::Missing) => {
            return Err(Box::new(refuse_as(
                StatusCode::CONFLICT,
                "not-live",
                format!("the live bench `{}` is not on disk any more, so leftovers cannot be worked out; make a bench live again", record.slug),
            )))
        }
        Err(LoadError::Unparseable(e)) => return Err(Box::new(refuse_as(StatusCode::CONFLICT, "unparseable", e))),
        // An I/O error is reported, never read as a broken file (the
        // round-1 rule GET and make-live follow too).
        Err(LoadError::Unreadable(e)) => return Err(Box::new(refuse(StatusCode::INTERNAL_SERVER_ERROR, e))),
    };
    if let Some(name) = record.config.as_deref().filter(|n| bench.config(n).is_none()) {
        return Err(Box::new(refuse_as(
            StatusCode::CONFLICT,
            "config-missing",
            format!("the live config `{name}` is no longer in `{}`, so which cables are on the desk cannot be told; make one of its configs live", record.slug),
        )));
    }
    Ok((bench, record.config.clone()))
}

/// `POST /api/bench/live/retract` — retracts **one leftover the current list
/// offers**, through the matching Core delete, and answers the leftovers
/// recomputed. A kind and id the list does not hold is refused `409`
/// (`not-offered`): retract only what was offered. When Core did not answer
/// the read for that kind, nothing is retracted and the answer says so.
pub async fn api_retract(State(state): State<AppState>, OpenRepo(repo): OpenRepo, Json(req): Json<RetractRequest>) -> Response {
    let _live = state.bench_live_lock.lock().await;
    let (bench, config_name) = match live_bench(&state, &repo) {
        Ok(b) => b,
        Err(resp) => return *resp,
    };
    let templates = catalog(Some(&repo)).by_id();
    let config = config_name.as_deref().and_then(|n| bench.config(n));
    let names = named(&bench, &templates, config);
    let before = leftovers(&state, &names).await;
    if let Some(unread) = before.unread_for(&req.kind) {
        return Json(serde_json::json!({
            "ok": false,
            "text": format!("{} — whether {} `{}` is a leftover cannot be told, so nothing was retracted", unread.text, req.kind, req.id),
            "leftovers": before.leftovers,
        }))
        .into_response();
    }
    let Some(item) = before.leftovers.iter().find(|l| l.kind == req.kind && l.id == req.id) else {
        return refuse_as(
            StatusCode::CONFLICT,
            "not-offered",
            format!("`{}` `{}` is not among this bench's leftovers now, so it is not retracted; only a listed leftover is", req.kind, req.id),
        );
    };
    let result = match item.kind {
        "signal" => state.core.remove_signal(&item.id).await,
        "instrument" => state.core.remove_instrument(&item.id).await,
        "bench-board" => state.core.unenroll_probe_by_serial(&item.id).await,
        "role" => state.core.unenroll_probe(&item.id).await,
        "bootload" => state.core.clear_bootload_ports().await,
        other => Err(anyhow::anyhow!("no retraction exists for a `{other}`")),
    };
    let (ok, mut text) = match result {
        Ok(true) => (true, format!("retracted the {} `{}`", item.kind, item.id)),
        Ok(false) => (false, format!("Core held no {} `{}` any more", item.kind, item.id)),
        Err(e) => (false, format!("{e:#}")),
    };
    // A role's row held the bench's own board type too: deleting it removed
    // that, so it is written again — and said — rather than lost silently.
    if ok && item.kind == "role" {
        if let Some(role) = Role::ALL.iter().find(|r| r.as_str() == item.id) {
            let binding = files::role_binding_in(&bench.parts, &bench.cables, &config.map(|c| vec![c.clone()]).unwrap_or_default(), *role, &templates);
            if let Some(binding) = binding {
                let written = if binding.board.is_empty() {
                    None
                } else {
                    let chip = role_chip(&state, &item.id, &binding).await;
                    write_role_board(&state, &item.id, &binding, &chip).await
                };
                text.push_str(&match &written {
                    Some(BoardWrite::Applied { chip }) => format!("; its board type {} ({chip}) was written again, since this bench names it", binding.board),
                    Some(BoardWrite::Refused { error, .. }) => format!("; writing its board type {} again was refused: {error}", binding.board),
                    Some(BoardWrite::NoChip(sentence)) => format!("; its board type {} was not written again: {sentence}", binding.board),
                    None => String::new(),
                });
                // dev-bench's link lived on the row the retract removed —
                // said whether or not the part names a board type.
                let link = (binding.link_port_serial.clone(), binding.link_port_interface);
                if *role == Role::DevBench && (link.0.is_some() || link.1.is_some()) {
                    text.push_str(&match written {
                        Some(BoardWrite::Applied { .. }) => match state.core.set_dev_bench_link(link.0.as_deref(), link.1).await {
                            Ok(()) => format!("; its link ({}) was declared again", link_text(&link.0, link.1)),
                            Err(e) => format!("; declaring its link ({}) again was refused: {e:#}", link_text(&link.0, link.1)),
                        },
                        _ => format!("; its link ({}) went with the row", link_text(&link.0, link.1)),
                    });
                }
            }
        }
    }
    let after = leftovers(&state, &names).await;
    for r in after.unread_rows() {
        text.push_str(&format!("; {}", r.text));
    }
    state.poke.notify_one();
    Json(serde_json::json!({ "ok": ok, "text": text, "leftovers": after.leftovers })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ebm-live-{tag}-{}-{}", std::process::id(), now_utc_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_record_path_sits_beside_the_ui_state_and_never_in_a_repo() {
        assert_eq!(
            record_path_from(Some("/x/state/recent-projects.json".into()), || Err("unused".into())),
            Some(PathBuf::from("/x/state/live-benches.json"))
        );
        assert_eq!(record_path_from(None, || Ok(PathBuf::from("/home/u/.local/share/embarch"))), Some(PathBuf::from("/home/u/.local/share/embarch/ui/live-benches.json")));
        assert_eq!(record_path_from(None, || Err("no data dir".into())), None);
        assert_eq!(record_path_from(Some("recent.json".into()), || Err("unused".into())), Some(PathBuf::from("live-benches.json")));
    }

    #[test]
    fn one_bench_is_live_per_machine_and_a_takeover_marks_the_other_repo() {
        let dir = temp("record");
        let path = dir.join("state").join("live-benches.json");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_eq!(record_for(&path, &a).unwrap(), None);
        let first = take_machine(&path, &a, "one", Some("A")).unwrap();
        // Another spelling of the same checkout is the same entry, and a re-apply keeps `since`.
        assert_eq!(take_machine(&path, &a.join("."), "one", Some("A")).unwrap().since_utc_ms, first.since_utc_ms);
        take_machine(&path, &b, "two", None).unwrap();
        let records = read_records(&path).unwrap();
        assert_eq!(records.machine.as_ref().unwrap().repo, repo_key(&b));
        assert_eq!(superseded(&records, &repo_key(&a)), Some(Superseded { repo: repo_key(&b), slug: "two".into(), config: None }));
        assert_eq!(superseded(&records, &repo_key(&b)), None);
        // Clearing `b` frees the machine; `a` stays superseded until it takes the machine back.
        clear_record(&path, &b).unwrap();
        let records = read_records(&path).unwrap();
        assert_eq!(records.machine, None);
        assert!(superseded(&records, &repo_key(&a)).is_some());
        let back = take_machine(&path, &a, "one", Some("A")).unwrap();
        assert_eq!(back.superseded_by, None);
        assert!(back.since_utc_ms >= first.since_utc_ms);
        // Clearing a repo the machine does not point at leaves the machine alone.
        take_machine(&path, &b, "two", None).unwrap();
        clear_record(&path, &a).unwrap();
        assert_eq!(read_records(&path).unwrap().machine.unwrap().repo, repo_key(&b));
        // The machine entry wins over an older mark: `a` was taken over by `b`,
        // and `c` then took the machine from `b`.
        let c = dir.join("c");
        std::fs::create_dir_all(&c).unwrap();
        take_machine(&path, &a, "one", None).unwrap();
        take_machine(&path, &b, "two", None).unwrap();
        take_machine(&path, &c, "three", None).unwrap();
        assert_eq!(superseded(&read_records(&path).unwrap(), &repo_key(&a)).unwrap().repo, repo_key(&c));
        // The first phase-4 build's bare map of repos does not parse, and is not overwritten.
        let old = format!("{{ \"{}\": {{ \"slug\": \"one\", \"config\": null, \"since_utc_ms\": 1 }} }}", repo_key(&a));
        std::fs::write(&path, &old).unwrap();
        assert!(read_records(&path).unwrap_err().contains("does not parse"));
        assert!(take_machine(&path, &a, "one", None).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(record_for(&path, &b).unwrap_err().contains("does not parse"));
        assert!(clear_record(&path, &b).is_err(), "an unreadable record is not replaced by an empty one");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binding_with_no_vid_and_pid_is_not_a_port_core_can_find() {
        let port = Binding::Port(files::PortBinding { vid: Some(0x1a86), pid: Some(0x7523), serial: None, interface: Some(0) });
        assert_eq!(port_of(&port, "x").unwrap(), UsbPortId { vid: 0x1a86, pid: 0x7523, serial: None, interface: Some(0) });
        assert!(port_of(&Binding::Serial("A1".into()), "load · usb's scpi binding").unwrap_err().contains("names no VID and PID"));
        assert!(port_of(&Binding::Interface(2), "x").is_err());
    }
}
