//! The Topology tab's server half: the project-level **board catalog**, the
//! saved **topology profiles**, and the one-pass **validation** that replaced
//! the alert list (decision 44).
//!
//! # Three facts, three owners
//!
//! A role — `dut` or `dev-bench`, and nothing else — is a slot on the bench,
//! and which probe currently fills it is Core's machine-wide
//! `enrollment.toml`. What a board is *called* is a project fact: a name, a
//! chip, and the west target it builds as, in the firmware repo's
//! `embarch/boards.toml` beside `embarch/studies/`. A topology profile
//! (`embarch/topologies/<slug>.toml`) is the third: which named board played
//! which role, on which probe, with which signals declared — a bench
//! description checked in next to the studies that ran on it.
//!
//! **Nothing here reads or writes hardware, and applying a profile does not
//! enrol.** Enrolling attaches a probe and reads a live hardware ID; a file
//! on disk cannot stand in for that, so a loaded profile *proposes* each
//! enrolment and a human confirms it through the ordinary enroll path. The
//! halves that carry no identity claim — the declared signals and the
//! dev-bench link — apply straight away, because re-stating a declaration is
//! all they ever were.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use embarch_core_client::SignalLink;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

/// The two roles, in the order the diagram draws them. The same pair
/// `embarch_topology::hardware::CANONICAL_ROLES` holds; spelled again here
/// rather than depended on, because this crate links that crate only
/// transitively and only for `wire` types (spec.md, "Shape") — and Core is
/// what enforces the vocabulary either way (its `POST /probes/enroll`
/// answers `400`).
pub const ROLES: [&str; 2] = ["dev-bench", "dut"];

/// How a role is *shown*. The wire spelling stays lowercase everywhere it is
/// sent; a report line that said `Role dut` would read as a name someone
/// typed, which is the exact confusion decision 44 separates out.
fn role_label(role: &str) -> &str {
    match role {
        "dut" => "DUT",
        "dev-bench" => "Dev bench",
        other => other,
    }
}

/// The board types `embarch-dev-bench`'s firmware supports, served from
/// here rather than restated in `app.js` — the same rule every other
/// vocabulary in this UI is under (spec.md, "A limit enforced server-side
/// is *served*").
///
/// **A dev bench is not a project's board.** The DUT box picks from the
/// open repo's catalog, because what a DUT is is that repo's business; the
/// bench is a piece of the suite, and offering a board the bench firmware
/// cannot be built for would be offering a bench that cannot exist.
pub const SUPPORTED_DEV_BENCH_BOARDS: [(&str, &str, &str); 2] = [
    // (board type as west names it, the label a human reads, probe-rs chip)
    ("nrf54l15dk/nrf54l15/cpuapp", "Nordic nRF54L15 DK", "nRF54L15"),
    ("esp32c5_devkitc/esp32c5/hpcore", "Espressif ESP32-C5-DevKitC", "esp32c5"),
];

/// How a dev-bench board type is *shown*: the label above, never the west
/// qualifier it is keyed by.
///
/// The wire value stays the qualifier everywhere it is sent — it is what
/// Core stores and what a build is for — but `nrf54l15dk/nrf54l15/cpuapp`
/// on a diagram box is a path, not a board, and a human reading the picture
/// wants to know which DK is on the desk. A type this list does not carry is
/// returned unchanged rather than blanked: an unknown bench board is still
/// the bench's board.
pub fn dev_bench_label(board: &str) -> &str {
    SUPPORTED_DEV_BENCH_BOARDS
        .iter()
        .find(|(qualifier, _, _)| *qualifier == board)
        .map(|(_, label, _)| *label)
        .unwrap_or(board)
}

/// The board type in a role, as a report line spells it: the bench's label
/// for a bench board, the name a human typed for anything else. A DUT's
/// name is already a name someone chose, so it is never translated.
fn board_display(role: &str, name: &str) -> String {
    if role == "dev-bench" { dev_bench_label(name).to_string() } else { name.to_string() }
}

// ---- the board catalog ------------------------------------------------------

/// One physical board a human owns, as `embarch/boards.toml` records it.
///
/// **No probe serial, deliberately.** A probe is moved between boards — one
/// J-Link serves three of them over a week — so a probe serial is a fact
/// about *the current wiring*, which is the enrolment's business (and the
/// profile's), never the board's. What stays with the board is what stays
/// true when it sits in a drawer: what it is called, what silicon it is, and
/// what it builds as.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Board {
    /// The name shown wherever this board appears, and the string an
    /// enrolment carries in `EnrolledBoard::name`. Unique within the file,
    /// case-sensitively: it is a label a human typed, not an identifier.
    pub name: String,
    /// The probe-rs target this board attaches as (`nRF54L15`,
    /// `STM32F407VG`) — what enrolling, flashing and every gate check need.
    #[serde(default)]
    pub chip: String,
    /// The west board target (`nrf54l15dk/nrf54l15/cpuapp`), so picking a
    /// board can fill the Build card rather than having it retyped.
    /// **This is the *west* board, a different thing from the physical board
    /// this record is** — the two have shared the word `board` in this UI
    /// since before the catalog existed, which is why this field is not
    /// called `board`.
    #[serde(default)]
    pub build_target: String,
    #[serde(default)]
    pub variant: String,
    #[serde(default)]
    pub revision: String,
    /// Free text for the human who owns the bench — which drawer, which
    /// cable, what is wrong with it. Rendered, never parsed.
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Catalog {
    #[serde(default)]
    boards: Vec<Board>,
}

fn boards_path(repo: &Path) -> PathBuf {
    repo.join("embarch").join("boards.toml")
}

fn topologies_dir(repo: &Path) -> PathBuf {
    repo.join("embarch").join("topologies")
}

/// Reads the catalog. **A missing file is an empty catalog; an unreadable or
/// unparseable one is an error** — the same split the signal list is under
/// (decision 10): "nothing declared" and "could not be read" are different
/// facts, and folding the second into the first states something about the
/// bench that was never established.
fn load_catalog(repo: &Path) -> anyhow::Result<Catalog> {
    let path = boards_path(repo);
    if !path.exists() {
        return Ok(Catalog::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
}

fn save_catalog(repo: &Path, catalog: &Catalog) -> anyhow::Result<()> {
    let path = boards_path(repo);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }
    let text = toml::to_string_pretty(catalog)?;
    std::fs::write(&path, text)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
}

// ---- what this repo can build a board for -----------------------------------

/// One buildable combination of a west board, as this repo's own target
/// scan reports it.
///
/// **A combination, not a cross product.** `revisions` and `variants` below
/// are the axes a human reads at a glance; this is what actually exists
/// together — `ref_board@evt1` may be buildable only *with* a named variant,
/// and a picker offering the two lists independently would offer a target
/// `west build` then refuses. That refusal is the whole reason the DUT
/// picker binds to one of these rather than to two dropdowns.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct BoardCombo {
    /// Empty where the board declares no revisions at all — a real state,
    /// not a missing value, and shown as one.
    pub revision: String,
    pub variant: String,
    pub soc: String,
    pub cpucluster: String,
    /// `board[@revision]/soc[/cpucluster][/variant]`: exactly what
    /// `west build -b` takes, assembled once here so nothing downstream
    /// re-derives it from the parts.
    pub qualifier: String,
}

/// What a catalog row can be built as, summarised for the list and enumerated
/// for the picker.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BoardBuilds {
    pub revisions: Vec<String>,
    pub variants: Vec<String>,
    pub combos: Vec<BoardCombo>,
}

/// The west board a catalog row is scanned under: `build_target` when it has
/// one, else the row's own name, cut back to the bare board.
///
/// A `build_target` is a full qualifier (`nrf54l15dk/nrf54l15/cpuapp`) and
/// the scan reports the board alone (`nrf54l15dk`), so the SoC, cpucluster
/// and any `@revision` are stripped before the two are compared. Comparing
/// them whole is what made an entry with a perfectly good target look absent
/// from its own repo's scan.
fn scan_key(board: &Board) -> String {
    let raw = if board.build_target.trim().is_empty() { &board.name } else { &board.build_target };
    raw.split('/').next().unwrap_or(raw).split('@').next().unwrap_or(raw).trim().to_string()
}

/// Groups the repo's scanned targets by west board.
///
/// Returns `None` with a reason when the repo cannot be scanned at all —
/// unconfigured, not a west project, a project config that names a different
/// repo. **That is a state, not an error**: the catalog is still a catalog,
/// and the list says "not scanned" rather than claiming a board builds for
/// nothing. Same split every other unreadable source in this UI is under.
async fn scan_by_west_board(
    state: &AppState,
    repo: &Path,
) -> (Option<std::collections::BTreeMap<String, BoardBuilds>>, Option<String>) {
    let configured = state.build_config_path.clone();
    let scan_repo = repo.to_path_buf();
    let survey = tokio::task::spawn_blocking(move || {
        crate::firmware_build::survey(configured.as_deref(), &scan_repo)
    })
    .await;
    let survey = match survey {
        Ok(s) => s,
        Err(e) => return (None, Some(format!("the target scan panicked: {e:?}"))),
    };
    if !survey.available {
        return (
            None,
            Some(survey.reason.unwrap_or_else(|| "this repo cannot be scanned".to_string())),
        );
    }

    // `resolve::list_targets`' own object, read the way `app.js`'s build
    // picker reads it (`targets.targets[]`) rather than reshaped — one shape
    // for the whole suite (`BuildSurvey::targets`).
    let rows = survey
        .targets
        .as_ref()
        .and_then(|t| t.get("targets"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let field = |row: &serde_json::Value, key: &str| -> String {
        row.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
    };

    let mut by_board: std::collections::BTreeMap<String, BoardBuilds> =
        std::collections::BTreeMap::new();
    // A set keyed by the combination itself, so two apps sharing one target
    // land in one entry rather than in two that read as two targets. **Which
    // apps those were is not kept** (decision 49): a board type's list of
    // apps was a column in the catalog and a suffix on every line of the
    // combination picker, and it is neither a thing a bench list is read for
    // nor a thing that picker chooses between — a study names its own app.
    let mut combos: std::collections::BTreeSet<(String, BoardCombo)> =
        std::collections::BTreeSet::new();
    for row in &rows {
        let board = field(row, "board");
        if board.is_empty() {
            continue;
        }
        let revision = field(row, "revision");
        let variant = field(row, "variant");
        let soc = field(row, "soc");
        let cpucluster = field(row, "cpucluster");
        let mut qualifier = board.clone();
        if !revision.is_empty() {
            qualifier.push('@');
            qualifier.push_str(&revision);
        }
        if !soc.is_empty() {
            qualifier.push('/');
            qualifier.push_str(&soc);
        }
        if !cpucluster.is_empty() {
            qualifier.push('/');
            qualifier.push_str(&cpucluster);
        }
        if !variant.is_empty() {
            qualifier.push('/');
            qualifier.push_str(&variant);
        }
        let combo = BoardCombo { revision, variant, soc, cpucluster, qualifier };
        combos.insert((board, combo));
    }

    for (board, combo) in combos {
        let builds = by_board.entry(board).or_default();
        if !combo.revision.is_empty() && !builds.revisions.contains(&combo.revision) {
            builds.revisions.push(combo.revision.clone());
        }
        if !combo.variant.is_empty() && !builds.variants.contains(&combo.variant) {
            builds.variants.push(combo.variant.clone());
        }
        builds.combos.push(combo);
    }
    for builds in by_board.values_mut() {
        builds.revisions.sort();
        builds.variants.sort();
    }
    (Some(by_board), None)
}

/// The scan keyed by **catalog name**, which is what every surface that
/// renders a catalog row already holds. Keying the served object by the west
/// board instead would hand the browser this file's `scan_key` rule to
/// re-implement, and a second implementation of a matching rule is how a row
/// comes to render as unbuildable in one place and buildable in another.
///
/// Kept typed rather than handed straight to `json!`, because the scan
/// answers a second question too: the SoC a row builds for, which is what
/// [`chips_from_scan`] turns into the probe-rs chip a row that names none
/// still attaches as (decision 48). Serving the JSON and then re-reading it
/// to find that out would be this crate parsing its own output.
struct CatalogBuilds {
    /// Empty when the repo could not be scanned at all, and empty when the
    /// scan simply found none of these rows — [`CatalogBuilds::available`]
    /// is what tells those two apart.
    by_board: std::collections::BTreeMap<String, BoardBuilds>,
    available: bool,
    reason: Option<String>,
}

impl CatalogBuilds {
    /// The shape `app.js` reads (`builds.available`, `builds.by_board`).
    fn to_json(&self) -> serde_json::Value {
        json!({
            "available": self.available,
            "reason": self.reason,
            "by_board": self.by_board,
        })
    }
}

async fn builds_by_catalog_name(
    state: &AppState,
    repo: &Path,
    boards: &[Board],
) -> CatalogBuilds {
    let (by_west_board, reason) = scan_by_west_board(state, repo).await;
    let Some(by_west_board) = by_west_board else {
        return CatalogBuilds { by_board: Default::default(), available: false, reason };
    };
    let mut by_board = std::collections::BTreeMap::new();
    for board in boards {
        if let Some(builds) = by_west_board.get(&scan_key(board)) {
            by_board.insert(board.name.clone(), builds.clone());
        }
    }
    CatalogBuilds { by_board, available: true, reason: None }
}

/// The probe-rs chip each catalog row attaches as, **for the rows that do
/// not state one** — keyed by catalog name, and absent for every row this
/// cannot answer for.
///
/// **A chip is not a second thing to type.** A row seeded by Rescan has an
/// empty `chip`, because the scan reports west targets; but it also reports
/// each target's *SoC*, and the SoC → probe-rs target table is already
/// Core's, compiled in and checked against probe-rs's own registry
/// ([`embarch-core` decision 8](../../embarch-doc/embarch-core/decisions.md),
/// `POST /resolve-chip`) — the same lookup a build for that target does on
/// its way to `/flash`. So the chip *is* known here, and a human retyping it
/// was only ever a chance to get it wrong. Decision 48.
///
/// **A hand-typed chip always wins**: a row that states one is not asked
/// about, so a corrected spelling is never overwritten by a derived one.
/// **Two SoCs under one board type abstain** rather than picking the first —
/// a board whose combinations do not agree about its silicon is exactly the
/// case where a guess attaches to the wrong target — and so does a Core that
/// cannot answer. Both come back absent, which the caller renders as the
/// unset field it already was.
async fn chips_from_scan(
    state: &AppState,
    boards: &[Board],
    builds: &CatalogBuilds,
) -> std::collections::BTreeMap<String, String> {
    let mut soc_by_row: std::collections::BTreeMap<String, String> = Default::default();
    for board in boards {
        if !board.chip.trim().is_empty() {
            continue;
        }
        let Some(scanned) = builds.by_board.get(&board.name) else { continue };
        let socs: std::collections::BTreeSet<&str> = scanned
            .combos
            .iter()
            .map(|c| c.soc.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if socs.len() == 1 {
            soc_by_row.insert(board.name.clone(), socs.into_iter().next().unwrap().to_string());
        }
    }

    // One round trip per distinct SoC, not per row: four board types on one
    // family is one question, and this runs on every load of the picker.
    let mut chip_by_soc: std::collections::BTreeMap<String, String> = Default::default();
    for soc in soc_by_row.values().cloned().collect::<std::collections::BTreeSet<String>>() {
        if let Ok(chip) = state.core.resolve_chip(&soc).await {
            chip_by_soc.insert(soc, chip);
        }
    }

    soc_by_row
        .into_iter()
        .filter_map(|(name, soc)| chip_by_soc.get(&soc).map(|chip| (name, chip.clone())))
        .collect()
}

/// The chip a board type attaches as, for a caller that holds only its name:
/// what the catalog states, else what the scan's SoC resolves to.
///
/// The slow path on purpose — it re-runs the target scan — because it exists
/// for a request that arrived *without* a chip, which is a human pressing a
/// button, not a poll.
pub async fn chip_for_board(state: &AppState, board: &str) -> Option<String> {
    let repo = state.study_designer.repo_path()?;
    let catalog = load_catalog(&repo).ok()?;
    if let Some(row) = catalog.boards.iter().find(|b| b.name == board) {
        if !row.chip.trim().is_empty() {
            return Some(row.chip.trim().to_string());
        }
    }
    let builds = builds_by_catalog_name(state, &repo, &catalog.boards).await;
    chips_from_scan(state, &catalog.boards, &builds).await.remove(board)
}

// ---- a saved topology profile -----------------------------------------------

/// One role's binding inside a profile: which named board, on which probe.
///
/// `chip` is recorded beside the board name even though the catalog holds it
/// too — a profile has to stay readable against a catalog that has since been
/// edited, and the chip a role was enrolled with is a fact about that
/// enrolment.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoleBinding {
    /// The catalog name of the board that held this role when the profile
    /// was saved. Empty when the board was enrolled unnamed.
    #[serde(default)]
    pub board: String,
    #[serde(default)]
    pub chip: String,
    #[serde(default)]
    pub probe_serial: String,
    /// dev-bench's runtime link, carried so a loaded profile can re-declare
    /// it. Meaningless for `dut`, and `None` there.
    #[serde(default)]
    pub link_port_serial: Option<String>,
    #[serde(default)]
    pub link_port_interface: Option<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    /// What the human called this bench. The file is named from a slug of
    /// it; this is the spelling shown.
    pub name: String,
    #[serde(default)]
    pub saved_at_utc_ms: u64,
    #[serde(default)]
    pub dut: Option<RoleBinding>,
    /// `dev-bench` on the wire and in the file, hyphen and all — the same
    /// spelling the role has everywhere else in the suite.
    #[serde(default, rename = "dev-bench")]
    pub dev_bench: Option<RoleBinding>,
    #[serde(default)]
    pub signals: Vec<SignalLink>,
}

/// A profile file name. Lowercase, and every run of anything else collapsed
/// to one hyphen, so a name a human typed cannot reach outside
/// `embarch/topologies/` — the file name is derived, never taken from input.
fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

fn profile_path(repo: &Path, slug: &str) -> PathBuf {
    topologies_dir(repo).join(format!("{slug}.toml"))
}

fn load_profile(repo: &Path, slug: &str) -> anyhow::Result<Profile> {
    let path = profile_path(repo, slug);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
}

/// Every profile in the project, newest first. **A file that will not parse
/// is listed with its error rather than skipped** — a topology the picker
/// silently omits is one a human goes looking for and cannot find.
fn list_profiles(repo: &Path) -> Vec<serde_json::Value> {
    let dir = topologies_dir(repo);
    let mut rows: Vec<(u64, serde_json::Value)> = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let slug = match path.file_stem().and_then(|x| x.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        match load_profile(repo, &slug) {
            Ok(profile) => rows.push((
                profile.saved_at_utc_ms,
                json!({
                    "slug": slug,
                    "name": profile.name,
                    "saved_at_utc_ms": profile.saved_at_utc_ms,
                    "dut": profile.dut,
                    "dev_bench": profile.dev_bench,
                    "signal_count": profile.signals.len(),
                }),
            )),
            Err(e) => rows.push((
                0,
                json!({ "slug": slug, "name": slug, "error": format!("{e:#}") }),
            )),
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    rows.into_iter().map(|(_, v)| v).collect()
}

fn now_utc_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// What a run should build for, taken from the role rather than from the
/// study (decision 45).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleTarget {
    pub board: String,
    pub variant: String,
    pub revision: String,
    /// The board type's own name, for the line that says where this came
    /// from.
    pub from_board: String,
}

/// The west target the board type in `role` builds as, or `None` when the
/// role holds no board or the catalog cannot say what it builds as.
///
/// **Two lookups, in this order.** A role's board type is a string Core
/// stores; what that type builds as is the project's business, so the
/// catalog is asked first. A dev-bench board falls back to the suite's
/// supported list, which carries the west target directly — a bench is not
/// a project's board and need not be in its catalog.
pub fn role_target(
    repo: Option<&Path>,
    enrolled: &[embarch_core_client::EnrolledBoardResponse],
    role: &str,
) -> Option<RoleTarget> {
    let row = enrolled.iter().find(|b| b.role == role)?;
    if row.name.is_empty() {
        return None;
    }
    if let Some(entry) = repo
        .and_then(|repo| load_catalog(repo).ok())
        .and_then(|catalog| catalog.boards.into_iter().find(|b| b.name == row.name))
    {
        let board = if entry.build_target.is_empty() { entry.name.clone() } else { entry.build_target };
        return Some(RoleTarget {
            board,
            variant: entry.variant,
            revision: entry.revision,
            from_board: row.name.clone(),
        });
    }
    if role == "dev-bench" {
        if let Some((board, _, _)) =
            SUPPORTED_DEV_BENCH_BOARDS.iter().find(|(board, _, _)| *board == row.name)
        {
            return Some(RoleTarget {
                board: (*board).to_string(),
                variant: String::new(),
                revision: String::new(),
                from_board: row.name.clone(),
            });
        }
    }
    // A board type Core holds that this project has never heard of. Not an
    // error and not a guess: the caller says so and builds what the study
    // asked for, which is the behaviour that existed before roles named a
    // board at all.
    None
}

// ---- routes -----------------------------------------------------------------

/// The open project's repo, or the `409` every route here answers without
/// one. **`409`, not `404`**: the catalog is not missing, the question is
/// unanswerable until a project is open — and the project picker at the
/// foot of the sidebar is the way out, which the message names.
fn repo(state: &AppState) -> Result<PathBuf, Box<Response>> {
    state.study_designer.repo_path().ok_or_else(|| {
        Box::new(
            (
                StatusCode::CONFLICT,
                "no firmware repo is open, so this project has no board catalog yet — pick one \
                 in Project, at the bottom of the sidebar",
            )
                .into_response(),
        )
    })
}

/// `GET /api/topology/boards` — the catalog, where it lives, and **what
/// each row can actually be built as**.
///
/// The list used to render a row's chip and its west target, which are the
/// two things a human reading a bench list already knows and cannot act on.
/// What is worth knowing is which revisions and variants this repo has in
/// the tree for that board — that is the menu a DUT is picked from — so the
/// scan rides along with the catalog rather than being a second call the
/// list would have to sequence itself behind.
pub async fn api_boards(State(state): State<AppState>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    match load_catalog(&repo) {
        Ok(catalog) => {
            let builds = builds_by_catalog_name(&state, &repo, &catalog.boards).await;
            Json(json!({
                "path": boards_path(&repo).to_string_lossy(),
                "boards": catalog.boards,
                "builds": builds.to_json(),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/topology/boards` — add a board, or replace the one with this
/// name. Idempotent by name, the same shape declaring a signal has, and for
/// the same reason: editing a board *is* re-stating it.
pub async fn api_save_board(State(state): State<AppState>, Json(board): Json<Board>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let name = board.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "a board needs a name").into_response();
    }
    let mut catalog = match load_catalog(&repo) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    };
    let board = Board { name: name.clone(), ..board };
    match catalog.boards.iter_mut().find(|b| b.name == name) {
        Some(existing) => *existing = board,
        None => catalog.boards.push(board),
    }
    catalog.boards.sort_by(|a, b| a.name.cmp(&b.name));
    match save_catalog(&repo, &catalog) {
        Ok(()) => Json(json!({ "saved": name, "boards": catalog.boards })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/topology/boards/rescan` — merge the board types this repo
/// actually builds for into the catalog.
///
/// **The scan seeds, the file wins.** Every board target the west scan
/// finds that the catalog does not already name is appended with its chip
/// left for a human (the scan knows west targets, not probe-rs ones); every
/// entry already in the file is left exactly as it is, including its chip,
/// notes and any hand-corrected spelling. A rescan can therefore only ever
/// *add* rows — it is the button for "I added a board to the repo", not a
/// regeneration that would quietly discard what someone typed.
///
/// A board type in the file that the scan no longer finds is **kept and
/// reported**, never deleted: a repo can build for a board on a branch that
/// is not checked out right now, and a catalog that silently shrank when
/// someone switched branches would be worse than one that is occasionally
/// generous.
pub async fn api_rescan_boards(State(state): State<AppState>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let configured = state.build_config_path.clone();
    let scan_repo = repo.clone();
    let survey = tokio::task::spawn_blocking(move || {
        crate::firmware_build::survey(configured.as_deref(), &scan_repo)
    })
    .await;
    let survey = match survey {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("the target scan panicked: {e:?}"))
                .into_response()
        }
    };
    if !survey.available {
        return (
            StatusCode::BAD_REQUEST,
            survey.reason.unwrap_or_else(|| "this repo cannot be scanned".to_string()),
        )
            .into_response();
    }

    let mut catalog = match load_catalog(&repo) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    };

    // `targets` is `resolve::list_targets`' own object, passed through
    // rather than reshaped (`BuildSurvey::targets`), so it is read here the
    // way the picker in `app.js` reads it: `targets.targets[].board`.
    let mut scanned: Vec<String> = survey
        .targets
        .as_ref()
        .and_then(|t| t.get("targets"))
        .and_then(|t| t.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("board").and_then(|b| b.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    scanned.sort();
    scanned.dedup();

    let mut added: Vec<String> = Vec::new();
    for board in &scanned {
        if catalog.boards.iter().any(|b| b.name == *board || b.build_target == *board) {
            continue;
        }
        catalog.boards.push(Board {
            name: board.clone(),
            build_target: board.clone(),
            ..Board::default()
        });
        added.push(board.clone());
    }
    let unseen: Vec<String> = catalog
        .boards
        .iter()
        .filter(|b| {
            let target = if b.build_target.is_empty() { &b.name } else { &b.build_target };
            !scanned.iter().any(|s| s == target)
        })
        .map(|b| b.name.clone())
        .collect();

    catalog.boards.sort_by(|a, b| a.name.cmp(&b.name));
    if let Err(e) = save_catalog(&repo, &catalog) {
        return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response();
    }
    Json(json!({
        "scanned": scanned,
        "added": added,
        "not_in_scan": unseen,
        "boards": catalog.boards,
    }))
    .into_response()
}

/// `DELETE /api/topology/boards/{name}` — forget a board.
///
/// **Removing a board from the catalog does not unenrol anything**, and says
/// so: the enrolment is Core's, keyed by role, and still names this board
/// afterwards. The name then renders as unresolved, which is honest — the
/// board is still on the bench.
pub async fn api_delete_board(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let mut catalog = match load_catalog(&repo) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    };
    let before = catalog.boards.len();
    catalog.boards.retain(|b| b.name != name);
    if catalog.boards.len() == before {
        return (StatusCode::NOT_FOUND, format!("no board named '{name}' in this project"))
            .into_response();
    }
    match save_catalog(&repo, &catalog) {
        Ok(()) => Json(json!({ "removed": name, "boards": catalog.boards })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct SetRoleBoardRequest {
    /// The board type. Empty is not a board: clearing which board is in a
    /// role is done by retracting the role, not by naming nothing.
    pub board: String,
    /// The probe-rs chip that board type attaches as. The browser sends
    /// what the picker's entry carries rather than deriving it, because the
    /// two lists that fill that picker (the project catalog and the
    /// supported-bench list) are both served from here.
    ///
    /// **Empty is answered here rather than relayed** (decision 48): the
    /// chip is looked up the same way the picker's entry got one, and only
    /// a board type nothing can answer for is refused.
    #[serde(default)]
    pub chip: String,
    /// The combination picked alongside the board type, when the picker
    /// offered one: a `(revision, variant)` pair the repo's own scan
    /// reports, never a pair typed from two independent lists.
    ///
    /// `None` leaves the catalog row alone, which is what a dev-bench pick
    /// and a repo that cannot be scanned both send. `Some("")` is a real
    /// value — a board with no revisions declared builds at none, and
    /// clearing a stale pin is the same gesture as setting one.
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub variant: Option<String>,
}

/// `POST /api/topology/roles/{role}/board` — which board type is in a role,
/// through Core's `PUT /probes/enrolled/{role}/board`.
///
/// A proxy for the same two reasons `/api/enroll` is: the write is Core's,
/// and the browser holds no bearer token. **It opens no probe** — that is
/// the whole point of the half it writes (decision 45).
pub async fn api_set_role_board(
    State(state): State<AppState>,
    axum::extract::Path(role): axum::extract::Path<String>,
    Json(req): Json<SetRoleBoardRequest>,
) -> Response {
    // **A chip nobody typed is looked up, not relayed as a refusal**
    // (decision 48). Core needs one — it is what every later attach on this
    // role opens as — and a request that arrives without one used to reach
    // it and come back as `502 … 400 Bad Request: a role's board needs both
    // a board type and the chip it attaches as`, which names the missing
    // field and not the one thing a human could do about it. The chip is
    // derivable from the SoC this repo's own scan reports, so it is derived;
    // what survives that is a genuinely unanswerable board type, refused
    // here as a `400` that says where to put the answer.
    let chip = match req.chip.trim() {
        "" => match chip_for_board(&state, &req.board).await {
            Some(chip) => chip,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "'{}' has no chip: it states none in embarch/boards.toml, and this \
                         repo's target scan does not report one SoC for it. Set the chip on \
                         the row in Board types below — Core attaches every probe bound to \
                         this role as it.",
                        req.board
                    ),
                )
                    .into_response()
            }
        },
        stated => stated.to_string(),
    };
    match state.core.set_role_board(&role, &req.board, &chip).await {
        Ok(row) => {
            // **Core's write first, the project file second, and only ever
            // in that order.** Which board type is in a role is Core's fact;
            // which combination of it this repo builds is the project's
            // (decision 44). Writing the file first would leave a catalog
            // pinned to a combination for a role that was then refused.
            let pinned = pin_combination(&state, &req);
            state.poke.notify_one();
            Json(json!({ "role": row, "pinned": pinned })).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// Records the picked combination on the catalog row, when one was picked.
///
/// Silent about a board that is not in the catalog and about a repo that is
/// not open: both are states the picker already renders honestly (a
/// dev-bench type is not a project board at all, and an unresolved name
/// shows as *not in catalog*), and neither is a reason to fail a write to
/// Core that has already happened.
fn pin_combination(state: &AppState, req: &SetRoleBoardRequest) -> serde_json::Value {
    let (Some(revision), Some(variant)) = (req.revision.as_ref(), req.variant.as_ref()) else {
        return serde_json::Value::Null;
    };
    let Some(repo) = state.study_designer.repo_path() else {
        return serde_json::Value::Null;
    };
    let Ok(mut catalog) = load_catalog(&repo) else {
        return serde_json::Value::Null;
    };
    let Some(entry) = catalog.boards.iter_mut().find(|b| b.name == req.board) else {
        return serde_json::Value::Null;
    };
    entry.revision = revision.clone();
    entry.variant = variant.clone();
    let pinned = json!({ "board": req.board, "revision": revision, "variant": variant });
    match save_catalog(&repo, &catalog) {
        Ok(()) => pinned,
        Err(e) => json!({ "board": req.board, "error": format!("{e:#}") }),
    }
}

/// `GET /api/topology/pickers` — what each role's board picker offers.
///
/// Two lists with two different owners, and the split is the decision
/// (45): the DUT's comes from the open project, because what a DUT is is
/// that repo's business, and the dev bench's is the suite's fixed
/// supported set. Served rather than restated in `app.js`, like every other
/// vocabulary here.
pub async fn api_pickers(State(state): State<AppState>) -> Response {
    let mut builds = json!({ "available": false, "reason": serde_json::Value::Null, "by_board": {} });
    let dut: Vec<serde_json::Value> = match state.study_designer.repo_path() {
        Some(repo) => match load_catalog(&repo) {
            Ok(catalog) => {
                let scanned = builds_by_catalog_name(&state, &repo, &catalog.boards).await;
                // The chip a row does not state, derived from the SoC the
                // scan reports (decision 48) — served here rather than in
                // each surface, because *both* of the dialogs that attach a
                // role to silicon read their chip out of this one list: the
                // board-type picker and the enroll dialog's read-only chip
                // field. A row with no chip used to make one of them refuse
                // in the browser and the other fail at Core.
                let derived = chips_from_scan(&state, &catalog.boards, &scanned).await;
                builds = scanned.to_json();
                catalog
                    .boards
                    .iter()
                    .map(|b| {
                        let stated = b.chip.trim();
                        let (chip, source) = if !stated.is_empty() {
                            (stated.to_string(), "catalog")
                        } else {
                            match derived.get(&b.name) {
                                Some(chip) => (chip.clone(), "scan"),
                                None => (String::new(), "none"),
                            }
                        };
                        json!({
                            "board": b.name,
                            "label": b.name,
                            "chip": chip,
                            // Where that chip came from, so a surface can
                            // say "from this repo's scan" instead of
                            // presenting a derived value as something a
                            // human typed.
                            "chip_source": source,
                            // What this row is *currently* pinned to in
                            // `boards.toml`, so the picker opens on the
                            // combination a build would use today rather
                            // than on whichever one sorts first.
                            "revision": b.revision,
                            "variant": b.variant,
                        })
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };
    let dev_bench: Vec<serde_json::Value> = SUPPORTED_DEV_BENCH_BOARDS
        .iter()
        .map(|(board, label, chip)| {
            json!({ "board": board, "label": label, "chip": chip, "chip_source": "suite" })
        })
        .collect();
    Json(json!({ "dut": dut, "dev-bench": dev_bench, "builds": builds })).into_response()
}

/// `DELETE /api/enrolled/{role}` — retract a role, through Core's
/// `DELETE /probes/enrolled/{role}`.
///
/// A proxy for the same two reasons `/api/enroll` is one: this handler holds
/// Core's bearer token and the browser does not, and Core owns the write.
/// The role is taken verbatim, including one outside the canonical pair —
/// clearing exactly those is what this route is for.
pub async fn api_unenroll(
    State(state): State<AppState>,
    axum::extract::Path(role): axum::extract::Path<String>,
) -> Response {
    match state.core.unenroll_probe(&role).await {
        Ok(true) => {
            state.poke.notify_one();
            Json(json!({ "removed": role })).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            format!("no board is enrolled under the role '{role}'"),
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct LinkRequest {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub interface: Option<u8>,
}

/// `POST /api/topology/link` — declares dev-bench's runtime link, through
/// Core's `POST /dev-bench/link`.
///
/// Its caller is applying a profile: the link is one of the two halves that
/// carry no identity claim, so it is re-declared rather than proposed. Core
/// requires dev-bench to be enrolled first, which is why the apply report
/// below leaves this call to the confirmation step when the role is empty.
pub async fn api_link(State(state): State<AppState>, Json(req): Json<LinkRequest>) -> Response {
    match state.core.set_dev_bench_link(req.serial.as_deref(), req.interface).await {
        Ok(()) => {
            state.poke.notify_one();
            Json(json!({ "linked": true })).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ---- validation -------------------------------------------------------------

/// A row's probe serial, or a phrase saying it has none — a role can hold
/// a board type with nothing bound to it (decision 45), and "probe " with
/// an empty string after it reads as a bug rather than as a state.
fn probe_text(board: &embarch_core_client::EnrolledBoardResponse) -> &str {
    board.probe_serial.as_deref().unwrap_or("(none bound)")
}

/// One line of the validation report. `status` is `pass`, `fail`, `warn` or
/// `empty`, and each means something a human acts on differently: `fail` is
/// something that is wrong, `warn` is something that cannot be asserted,
/// `empty` is a slot nothing is in. **`warn` is never rendered as a pass** —
/// the whole reason this replaced the alert list is that a validate pass says
/// what it checked.
#[derive(Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: &'static str,
    pub detail: String,
    /// Core's own words, verbatim, when there are any — a mismatch reason, a
    /// `fix_it_url`, the text of a refusal. Never paraphrased: the failure a
    /// human has to act on is the one Core described.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

impl Check {
    fn new(id: &str, label: String, status: &'static str, detail: String) -> Check {
        Check { id: id.to_string(), label, status, detail, log: None }
    }
}

/// `POST /api/topology/validate` — one pass over the whole declared
/// topology, run when a human asks for it.
///
/// **This is what the Alerts card became** (decision 44). The alert log
/// answered "did a mismatch happen at some point", a question nobody was
/// asking at the moment they looked at it, and it stayed silent about a
/// signal route entirely (`embarch-topology` spec.md: a signal mismatch is
/// never written to the alert log, because an alert's shape is board-
/// specific and a wire has none of those fields). This answers "is the bench
/// in front of me what this tab says it is", check by check, now.
///
/// Three kinds of check, and the difference between them is what each can
/// honestly assert:
///
/// - **A role** re-reads the enrolled board's live hardware ID over the
///   probe (Core's `POST /validate`) — the strongest thing here, and the
///   only one that touches hardware.
/// - **The dev-bench link** resolves the port live, and reports a *guessed*
///   port as a warning rather than a pass: a port picked by the
///   lowest-interface fallback is exactly the failure mode
///   `link_port_interface` exists for.
/// - **A signal route** can only be checked as far as it was declared: a
///   `direct` route's serial must still be enumerable on Core's machine, and
///   a `via-dev-bench` one validates on the strength of being declared,
///   since its carrier is the bench link the check above already covers.
///   Neither can confirm a wire between two headers, and the report says so
///   rather than implying a pass means the cable is there.
pub async fn api_validate(State(state): State<AppState>) -> Response {
    let snapshot = state.snapshot_rx.borrow().clone();
    if !snapshot.core_reachable {
        return (
            StatusCode::BAD_GATEWAY,
            snapshot
                .error
                .unwrap_or_else(|| "embarch-core is unreachable".to_string()),
        )
            .into_response();
    }

    let mut checks: Vec<Check> = Vec::new();

    // ---- the two roles ----
    for role in ROLES {
        let enrolled = snapshot.enrolled.iter().find(|b| b.role == role);
        let Some(board) = enrolled else {
            checks.push(Check::new(
                &format!("role:{role}"),
                format!("Role {}", role_label(role)),
                "empty",
                "no board is enrolled under this role — drop a probe on its box to enrol one"
                    .to_string(),
            ));
            continue;
        };
        if board.name.is_empty() {
            checks.push(Check::new(
                &format!("role:{role}"),
                format!("Role {}", role_label(role)),
                "empty",
                "no board type is in this role — pick one on the diagram, so a run knows what to \
                 build for"
                    .to_string(),
            ));
            continue;
        }
        if board.probe_serial.is_none() {
            checks.push(Check::new(
                &format!("role:{role}"),
                format!("Role {}", role_label(role)),
                "empty",
                format!(
                    "{} is in this role, but no probe is bound to it — drop one on the box \
                     before anything can read this board's identity",
                    board_display(role, &board.name)
                ),
            ));
            continue;
        }
        let who = format!(
            "{} — probe {} ({})",
            board_display(role, &board.name),
            probe_text(board),
            board.chip
        );
        match state.core.validate(role).await {
            Ok(resp) => {
                let mut check = Check::new(
                    &format!("role:{role}"),
                    format!("Role {}", role_label(role)),
                    "pass",
                    format!("{who}: live hardware ID still {}", resp.hardware_id),
                );
                // `validated_at_utc_ms` is this check's own instant, never
                // `confirmed_at_utc_ms`, which is enrolment time and does
                // not move on a re-check (`embarch-core` decision 57).
                if let Some(at) = resp.validated_at_utc_ms {
                    check.log = Some(format!("validated_at_utc_ms {at}"));
                }
                checks.push(check);
            }
            Err(e) => {
                let mut check = Check::new(
                    &format!("role:{role}"),
                    format!("Role {}", role_label(role)),
                    "fail",
                    format!("{who}: Core refused this role"),
                );
                check.log = Some(format!("{e:#}"));
                checks.push(check);
            }
        }
    }

    // ---- anything enrolled outside the pair ----
    for board in snapshot.enrolled.iter().filter(|b| !ROLES.contains(&b.role.as_str())) {
        checks.push(Check::new(
            &format!("foreign:{}", board.role),
            format!("Leftover role '{}'", board.role),
            "warn",
            format!(
                "'{}' is not a role — the roles are {}. This is a board name written where a \
                 role belongs, from before the two were separate facts; clear the row and \
                 re-enrol the board under a real role.",
                board.role,
                ROLES.join(" and ")
            ),
        ));
    }

    // ---- dev-bench's runtime link ----
    match state.core.dev_bench_port().await {
        Ok(Some(port)) => {
            let status = if port.guessed_among.is_some() { "warn" } else { "pass" };
            let detail = match port.guessed_among {
                Some(n) => format!(
                    "{} — guessed among {n}: nothing declared could narrow them, and the \
                     lowest-interface fallback is wrong on a two-VCOM probe. Declare the \
                     interface.",
                    port.port_name
                ),
                None => format!("{} ({})", port.port_name, port.detected_by),
            };
            checks.push(Check::new("dev-bench-link", "dev-bench link".to_string(), status, detail));
        }
        Ok(None) => checks.push(Check::new(
            "dev-bench-link",
            "dev-bench link".to_string(),
            "empty",
            "Core resolved no dev-bench port — the bench is unplugged, or its link is not \
             declared"
                .to_string(),
        )),
        Err(e) => {
            let mut check = Check::new(
                "dev-bench-link",
                "dev-bench link".to_string(),
                "fail",
                "Core could not resolve the dev-bench port".to_string(),
            );
            check.log = Some(format!("{e:#}"));
            checks.push(check);
        }
    }

    // ---- every declared signal's carrier ----
    if let Some(err) = &snapshot.signals_error {
        let mut check = Check::new(
            "signals",
            "Declared signals".to_string(),
            "warn",
            "Core did not answer the signal list, so no route here was checked".to_string(),
        );
        check.log = Some(err.clone());
        checks.push(check);
    }
    for signal in &snapshot.signals {
        let id = format!("signal:{}", signal.name);
        match &signal.route {
            embarch_core_client::SignalRoute::Direct { port_serial } => {
                let found = snapshot
                    .serial_ports
                    .iter()
                    .find(|p| p.serial_number.as_deref() == Some(port_serial.as_str()));
                match found {
                    Some(port) => checks.push(Check::new(
                        &id,
                        format!("Signal {}", signal.name),
                        "pass",
                        format!(
                            "direct on {} ({port_serial}) — enumerable now. This does not \
                             confirm the wire itself: no software can see a cable between two \
                             headers.",
                            port.port_name
                        ),
                    )),
                    None if snapshot.serial_ports_error.is_some() => {
                        let mut check = Check::new(
                            &id,
                            format!("Signal {}", signal.name),
                            "warn",
                            format!(
                                "direct on {port_serial} — Core did not answer its port \
                                 enumeration, so this route was not checked"
                            ),
                        );
                        check.log = snapshot.serial_ports_error.clone();
                        checks.push(check);
                    }
                    None => checks.push(Check::new(
                        &id,
                        format!("Signal {}", signal.name),
                        "fail",
                        format!(
                            "direct on {port_serial} — nothing with that USB serial is \
                             enumerated on Core's machine"
                        ),
                    )),
                }
            }
            embarch_core_client::SignalRoute::ViaDevBench { rx_pin, tx_pin } => {
                checks.push(Check::new(
                    &id,
                    format!("Signal {}", signal.name),
                    "pass",
                    format!(
                        "via dev-bench, rx {rx_pin} / tx {tx_pin} — declared. Its carrier is \
                         the bench link checked above; the pins themselves are a wire, which \
                         no check can see."
                    ),
                ));
            }
        }
    }

    // ---- board types that no longer resolve ----
    //
    // **The dev bench is checked against the suite's list, not the
    // project's** (decision 45). A bench board type is the suite's fact —
    // it is what `embarch-dev-bench`'s firmware supports — so warning that
    // it is absent from a firmware repo's catalog would nag on every pass
    // about a file that was never meant to name it. Found on the live
    // bench, on the first pass after the board half was declared.
    if let Some(repo) = state.study_designer.repo_path() {
        if let Ok(catalog) = load_catalog(&repo) {
            for board in snapshot.enrolled.iter().filter(|b| !b.name.is_empty()) {
                let supported_bench = board.role == "dev-bench"
                    && SUPPORTED_DEV_BENCH_BOARDS.iter().any(|(b, _, _)| *b == board.name);
                if supported_bench {
                    continue;
                }
                if !catalog.boards.iter().any(|c| c.name == board.name) {
                    checks.push(Check::new(
                        &format!("name:{}", board.name),
                        format!("Board '{}'", board.name),
                        "warn",
                        format!(
                            "in the '{}' role, but no board type of that name is in this \
                             project's catalog — either it belongs to another project, or \
                             Rescan has not been pressed since it was added to the repo",
                            board.role
                        ),
                    ));
                }
            }
        }
    }

    let failed = checks.iter().filter(|c| c.status == "fail").count();
    let warned = checks.iter().filter(|c| c.status == "warn").count();
    Json(json!({
        "ok": failed == 0,
        "failed": failed,
        "warned": warned,
        "checked_at_utc_ms": now_utc_ms(),
        "checks": checks,
    }))
    .into_response()
}

// ---- profiles ---------------------------------------------------------------

/// `GET /api/topology/profiles` — every saved bench in this project.
pub async fn api_profiles(State(state): State<AppState>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    Json(json!({
        "dir": topologies_dir(&repo).to_string_lossy(),
        "profiles": list_profiles(&repo),
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SaveProfileRequest {
    pub name: String,
}

/// `POST /api/topology/profiles` — writes what the bench currently *is* to a
/// named file: both roles as enrolled (board name, chip, probe serial, and
/// dev-bench's link), and every declared signal.
///
/// Built from the snapshot rather than re-read from Core: the snapshot is
/// that same read, at most one poll old, and a save that quietly disagreed
/// with the diagram it was taken from would be worse than one a few seconds
/// stale.
pub async fn api_save_profile(
    State(state): State<AppState>,
    Json(req): Json<SaveProfileRequest>,
) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let name = req.name.trim().to_string();
    let slug = slugify(&name);
    if slug.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "a topology needs a name with at least one letter or digit in it",
        )
            .into_response();
    }

    let snapshot = state.snapshot_rx.borrow().clone();
    let binding = |role: &str| -> Option<RoleBinding> {
        snapshot.enrolled.iter().find(|b| b.role == role).map(|b| RoleBinding {
            board: b.name.clone(),
            chip: b.chip.clone(),
            // A role saved with no probe bound saves as one: a bench
            // description is worth keeping even before it is wired, and an
            // empty serial in the file would read as a probe named "".
            probe_serial: b.probe_serial.clone().unwrap_or_default(),
            link_port_serial: b.link_port_serial.clone(),
            link_port_interface: b.link_port_interface,
        })
    };

    let profile = Profile {
        name: name.clone(),
        saved_at_utc_ms: now_utc_ms(),
        dut: binding("dut"),
        dev_bench: binding("dev-bench"),
        signals: snapshot.signals.clone(),
    };

    let dir = topologies_dir(&repo);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (StatusCode::BAD_REQUEST, format!("failed to create {}: {e}", dir.display()))
            .into_response();
    }
    let text = match toml::to_string_pretty(&profile) {
        Ok(t) => t,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    };
    let path = profile_path(&repo, &slug);
    match std::fs::write(&path, text) {
        Ok(()) => Json(json!({
            "saved": slug,
            "path": path.to_string_lossy(),
            "profiles": list_profiles(&repo),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("failed to write {}: {e}", path.display()),
        )
            .into_response(),
    }
}

/// `DELETE /api/topology/profiles/{slug}`.
pub async fn api_delete_profile(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let slug = slugify(&slug);
    let path = profile_path(&repo, &slug);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, format!("no saved topology '{slug}'")).into_response();
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Json(json!({ "removed": slug, "profiles": list_profiles(&repo) })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/topology/profiles/{slug}/apply` — loads a saved bench.
///
/// **Two halves, and only one of them applies.** The signals and the
/// dev-bench link are declarations: re-stating them changes a file on Core
/// and asserts nothing about silicon, so they go in now, and each one's
/// outcome is reported individually rather than as one "applied". The
/// enrolments are identity claims — a role is bound to a probe *after* a live
/// hardware-ID read — so they come back as **proposals**, each carrying the
/// role, board name, chip and probe serial the file holds, for a human to
/// confirm through the ordinary enroll path. A file on disk is not evidence
/// about what is plugged in, and a load that enrolled straight from one would
/// be exactly the stale-declared-state failure this suite's topology crate
/// exists to prevent.
pub async fn api_apply_profile(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let slug = slugify(&slug);
    let profile = match load_profile(&repo, &slug) {
        Ok(p) => p,
        Err(e) => return (StatusCode::NOT_FOUND, format!("{e:#}")).into_response(),
    };

    let snapshot = state.snapshot_rx.borrow().clone();
    let mut applied: Vec<serde_json::Value> = Vec::new();

    for signal in &profile.signals {
        match state.core.declare_signal(signal).await {
            Ok(()) => applied.push(json!({
                "what": format!("signal {}", signal.name),
                "ok": true,
                "detail": "declared",
            })),
            Err(e) => applied.push(json!({
                "what": format!("signal {}", signal.name),
                "ok": false,
                "detail": format!("{e:#}"),
            })),
        }
    }

    // The link amends dev-bench's enrolment row, so Core refuses it when
    // that role is empty. Left to the confirmation step in that case, and
    // said out loud — a silent skip would leave a bench half-loaded with
    // nothing on screen about it.
    let bench_enrolled = snapshot.enrolled.iter().any(|b| b.role == "dev-bench");
    if let Some(bench) = &profile.dev_bench {
        if bench.link_port_serial.is_some() || bench.link_port_interface.is_some() {
            if bench_enrolled {
                match state
                    .core
                    .set_dev_bench_link(bench.link_port_serial.as_deref(), bench.link_port_interface)
                    .await
                {
                    Ok(()) => applied.push(json!({
                        "what": "dev-bench link",
                        "ok": true,
                        "detail": "declared",
                    })),
                    Err(e) => applied.push(json!({
                        "what": "dev-bench link",
                        "ok": false,
                        "detail": format!("{e:#}"),
                    })),
                }
            } else {
                applied.push(json!({
                    "what": "dev-bench link",
                    "ok": false,
                    "detail": "deferred: Core amends the dev-bench row, so the role has to hold \
                               a board first — confirm its enrolment below and the link is \
                               declared with it",
                }));
            }
        }
    }

    let mut proposals: Vec<serde_json::Value> = Vec::new();
    for (role, binding) in [("dev-bench", &profile.dev_bench), ("dut", &profile.dut)] {
        let Some(binding) = binding else { continue };
        // **The board half applies now, like the signals.** It names a
        // shape a repo builds for and claims nothing about silicon
        // (decision 45), so a loaded bench describes itself immediately;
        // only the probe binding below, which is an identity claim, waits
        // for a human.
        if !binding.board.is_empty() && !binding.chip.is_empty() {
            match state.core.set_role_board(role, &binding.board, &binding.chip).await {
                Ok(_) => applied.push(json!({
                    "what": format!("{role} board"),
                    "ok": true,
                    "detail": format!("{} ({})", binding.board, binding.chip),
                })),
                Err(e) => applied.push(json!({
                    "what": format!("{role} board"),
                    "ok": false,
                    "detail": format!("{e:#}"),
                })),
            }
        }
        if binding.probe_serial.is_empty() {
            continue;
        }
        let current = snapshot.enrolled.iter().find(|b| b.role == role);
        let already = current
            .is_some_and(|b| b.probe_serial.as_deref() == Some(binding.probe_serial.as_str()));
        let attached = snapshot
            .probes
            .iter()
            .any(|p| p.serial_number.as_deref() == Some(binding.probe_serial.as_str()));
        proposals.push(json!({
            "role": role,
            "board": binding.board,
            "chip": binding.chip,
            "probe_serial": binding.probe_serial,
            "link_port_serial": binding.link_port_serial,
            "link_port_interface": binding.link_port_interface,
            // Three separate facts, none of which is a decision: whether
            // this role already holds this probe, whether the probe is even
            // attached, and who would be displaced. The browser renders
            // them; nothing here skips a proposal on their account.
            "already_enrolled": already,
            "probe_attached": attached,
            "displaces": current.map(|b| json!({
                "board": b.name,
                "chip": b.chip,
                "probe_serial": b.probe_serial,
            })),
        }));
    }

    state.poke.notify_one();
    Json(json!({
        "slug": slug,
        "name": profile.name,
        "applied": applied,
        "proposals": proposals,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_cannot_escape_the_topologies_directory() {
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("Bench A"), "bench-a");
        assert_eq!(slugify("  bench/../a  "), "bench-a");
        assert_eq!(slugify("nrf54l15 + esp32"), "nrf54l15-esp32");
        assert_eq!(slugify("///"), "", "a name with nothing in it slugs to nothing, and is refused");
    }

    /// A catalog written by hand — the normal case, since this file is meant
    /// to be edited in a repo — loads with every optional field absent.
    #[test]
    fn a_minimal_catalog_entry_loads() {
        let catalog: Catalog = toml::from_str(
            r#"
[[boards]]
name = "client-nucleo"
chip = "STM32F407VG"
"#,
        )
        .expect("a board with only a name and a chip is a board");
        assert_eq!(catalog.boards.len(), 1);
        assert_eq!(catalog.boards[0].build_target, "");
        assert_eq!(catalog.boards[0].notes, "");
    }

    /// The role key keeps its hyphen in the file, so a profile is readable
    /// as the same word the rest of the suite uses.
    #[test]
    fn a_profile_round_trips_with_the_dev_bench_key_spelled_with_a_hyphen() {
        let profile = Profile {
            name: "bench a".to_string(),
            saved_at_utc_ms: 1_755_000_000_000,
            dut: Some(RoleBinding {
                board: "wearable-rev6".to_string(),
                chip: "nRF54L15".to_string(),
                probe_serial: "001057729826".to_string(),
                ..RoleBinding::default()
            }),
            dev_bench: Some(RoleBinding {
                board: "bench-nrf54l15dk".to_string(),
                chip: "nRF54L15".to_string(),
                probe_serial: "001050288460".to_string(),
                link_port_serial: Some("D607104".to_string()),
                link_port_interface: Some(2),
            }),
            signals: Vec::new(),
        };
        let text = toml::to_string_pretty(&profile).unwrap();
        assert!(text.contains("[dev-bench]"), "{text}");
        let back: Profile = toml::from_str(&text).unwrap();
        assert_eq!(back.dev_bench.unwrap().link_port_interface, Some(2));
        assert_eq!(back.dut.unwrap().board, "wearable-rev6");
    }
}
