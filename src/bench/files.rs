//! A bench on disk: `embarch/topologies/<slug>.toml`, the same file a saved
//! topology profile has always been (decision 44).
//!
//! **Today's files still load.** A file with no `[[parts]]` is *legacy*: its
//! `dut` and `dev-bench` bindings become parts laid out as the BLE dev bench —
//! this machine, the dev-bench board and the DUT board, each cabled to the
//! hub — and the file is rewritten in the new shape only when it is saved.
//! Once a file has parts, the old keys are not written; the topology routes
//! that still read them get them derived from the role parts instead
//! ([`profile_from_file`]).
//!
//! **`[[signals]]` stays `SignalLink` verbatim**, so the apply path reads the
//! same rows it always did.

use std::collections::BTreeMap;
use std::path::Path;

use embarch_core_client::SignalLink;
use serde::{Deserialize, Deserializer, Serialize};

use super::check::Templates;
use super::{builtins, version_of, write_in_repo, FileError, Function, Kind, Role, WriteError, WriteMode, GENERIC_BOARD, HUB_TEMPLATE};
use crate::topology::{profile_path, slugify, topologies_dir, Profile, RoleBinding};

/// Which USB identity a `usb-host` function is bound to, picked from Core's
/// enumeration.
///
/// Three spellings, because the identities differ: a probe is its serial
/// (`probe = "0671FF…"`); a VCOM on the same device as the connector's probe
/// is that device's interface (`vcom = 1`); a port of its own — a bridge, an
/// instrument's line — is a serial (`vcom = "FT9ABCDE"`) or a table naming
/// what tells it apart (`{ serial = "…", interface = 2 }`, or `vid`/`pid` for
/// a bridge with no serial at all, `embarch-topology` decision 49).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Binding {
    Interface(u8),
    Serial(String),
    Port(PortBinding),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
}

/// A position, rounded to whole units on the way in: a dragged part lands on
/// a fraction, and the file stores integers.
fn rounded_at<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[i32; 2]>, D::Error> {
    let raw: Option<[f64; 2]> = Option::deserialize(d)?;
    Ok(raw.map(|[x, y]| [x.round() as i32, y.round() as i32]))
}

/// A template placed in a bench.
///
/// **Loaded tolerantly**: an unknown key is ignored rather than refusing the
/// file, the same "closed on the write path, tolerant on the load path" rule
/// decision 44 set for roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub template: String,
    /// The name shown, and a bench board's enrolled name.
    #[serde(default)]
    pub label: Option<String>,
    /// `None` for a mounted part: it moves with its board.
    #[serde(default, deserialize_with = "rounded_at")]
    pub at: Option<[i32; 2]>,
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub mounted_on: Option<String>,
    #[serde(default)]
    pub flip: bool,
    /// The board type in this part's role (decision 45): a shape a repo
    /// builds for, never a unit.
    #[serde(default)]
    pub board: Option<String>,
    /// The chip a legacy file recorded for this role, **with the board it was
    /// recorded for**, kept when the file is converted. Only a fallback: apply
    /// and enrol look the chip up from the board type first, and use this when
    /// nothing answers (decision 48, "looked up, never typed") — and only
    /// while `board` still equals `chip.board`, so a chip never outlives its
    /// board. Written only when present.
    #[serde(default)]
    pub chip: Option<RecordedChip>,
    /// Connector id → function → the identity it is bound to.
    #[serde(default)]
    pub usb: BTreeMap<String, BTreeMap<Function, Binding>>,
}

impl Part {
    /// How a part is named in a sentence: its label, else its id.
    pub fn display(&self) -> &str {
        self.label.as_deref().filter(|l| !l.trim().is_empty()).unwrap_or(&self.id)
    }

    /// The probe serial bound on any of this part's connectors.
    pub fn probe_serial(&self) -> Option<&str> {
        self.usb.values().find_map(|functions| match functions.get(&Function::Probe) {
            Some(Binding::Serial(s)) => Some(s.as_str()),
            Some(Binding::Port(PortBinding { serial: Some(s), .. })) => Some(s.as_str()),
            _ => None,
        })
    }
}

/// A recorded chip and the board it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedChip {
    #[serde(default)]
    pub board: String,
    pub chip: String,
}

/// A bench-level cable: one end is this machine, and it is shared by every
/// config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cable {
    pub from: String,
    pub to: String,
}

/// A named set of part-to-part cables. Switching configs never calls Core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    #[serde(default)]
    pub cables: Vec<[String; 2]>,
}

/// A bench as the routes serve and accept it (the JSON contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bench {
    /// Derived from `name` on save, never taken from it.
    #[serde(default)]
    pub slug: String,
    pub name: String,
    /// Read from a file with no `[[parts]]`; its parts were laid out from its
    /// `dut`/`dev-bench`. Ignored on save, which writes the new shape.
    #[serde(default)]
    pub legacy: bool,
    /// The file's version as it was read (`super::version_of`), which an
    /// update must name. `None` for a bench never saved.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub cables: Vec<Cable>,
    #[serde(default)]
    pub configs: Vec<Config>,
    #[serde(default)]
    pub signals: Vec<SignalLink>,
}

impl Bench {
    pub fn config(&self, name: &str) -> Option<&Config> {
        self.configs.iter().find(|c| c.name == name)
    }
}

/// A bench file as TOML holds it: today's shape, and the old keys a legacy
/// file still carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchFile {
    pub name: String,
    #[serde(default)]
    pub saved_at_utc_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dut: Option<RoleBinding>,
    #[serde(default, rename = "dev-bench", skip_serializing_if = "Option::is_none")]
    pub dev_bench: Option<RoleBinding>,
    /// `None` — the key absent — is what makes a file legacy. A saved bench
    /// always writes it, even empty.
    #[serde(default)]
    pub parts: Option<Vec<Part>>,
    #[serde(default)]
    pub cables: Vec<Cable>,
    #[serde(default)]
    pub configs: Vec<Config>,
    #[serde(default)]
    pub signals: Vec<SignalLink>,
}

// ---- legacy files -----------------------------------------------------------

/// The hub's part id in a laid-out legacy bench.
pub const LEGACY_HUB: &str = "host";

/// Where a legacy bench's three parts go: the BLE dev bench's layout, this
/// machine on the left and the two boards to its right.
const LEGACY_AT: [(&str, [i32; 2]); 3] = [(LEGACY_HUB, [20, 110]), ("dev-bench", [330, 110]), ("dut", [700, 110])];

fn legacy_at(id: &str) -> Option<[i32; 2]> {
    LEGACY_AT.iter().find(|(i, _)| *i == id).map(|(_, at)| *at)
}

/// The built-in a board type is drawn as: the part its west board names when
/// there is one, the generic board otherwise.
fn template_for_board(board: &str) -> &'static str {
    let west = board.split('/').next().unwrap_or(board).split('@').next().unwrap_or(board);
    match west {
        "nrf54l15dk" => "nrf54l15dk",
        "nucleo_g0b1re" => "nucleo-g0b1re",
        _ => GENERIC_BOARD,
    }
}

/// The `usb-host` connector a template's probe rides on.
fn probe_connector(template: &str) -> Option<String> {
    builtins()
        .into_iter()
        .find(|t| t.id == template)?
        .connectors
        .into_iter()
        .find(|c| c.kind == Kind::UsbHost && c.carries(Function::Probe))
        .map(|c| c.id)
}

/// A role binding from an old file, as a part.
fn legacy_part(id: &str, role: Role, binding: &RoleBinding) -> (Part, Option<Cable>) {
    let board = binding.board.trim();
    let template = template_for_board(board);
    let connector = probe_connector(template);
    let mut usb = BTreeMap::new();
    if let Some(connector) = &connector {
        let mut functions = BTreeMap::new();
        if !binding.probe_serial.is_empty() {
            functions.insert(Function::Probe, Binding::Serial(binding.probe_serial.clone()));
        }
        let vcom = match (&binding.link_port_serial, binding.link_port_interface) {
            (Some(s), Some(i)) => Some(Binding::Port(PortBinding { serial: Some(s.clone()), interface: Some(i), ..Default::default() })),
            (Some(s), None) => Some(Binding::Serial(s.clone())),
            (None, Some(i)) => Some(Binding::Interface(i)),
            (None, None) => None,
        };
        if let Some(vcom) = vcom {
            functions.insert(Function::Vcom, vcom);
        }
        if !functions.is_empty() {
            usb.insert(connector.clone(), functions);
        }
    }
    let part = Part {
        id: id.to_string(),
        template: template.to_string(),
        label: None,
        at: legacy_at(id),
        role: Some(role),
        mounted_on: None,
        flip: false,
        board: (!board.is_empty()).then(|| board.to_string()),
        chip: Some(binding.chip.trim().to_string())
            .filter(|c| !c.is_empty())
            .map(|chip| RecordedChip { board: board.to_string(), chip }),
        usb,
    };
    let cable = connector.map(|c| Cable { from: format!("{id}.{c}"), to: LEGACY_HUB.to_string() });
    (part, cable)
}

/// The parts and bench-level cables a legacy file's bindings describe.
pub fn legacy_layout(dut: Option<&RoleBinding>, dev_bench: Option<&RoleBinding>) -> (Vec<Part>, Vec<Cable>) {
    let mut parts = vec![Part {
        id: LEGACY_HUB.to_string(),
        template: HUB_TEMPLATE.to_string(),
        label: None,
        at: legacy_at(LEGACY_HUB),
        role: None,
        mounted_on: None,
        flip: false,
        board: None,
        chip: None,
        usb: BTreeMap::new(),
    }];
    let mut cables = Vec::new();
    for (id, role, binding) in [("dev-bench", Role::DevBench, dev_bench), ("dut", Role::Dut, dut)] {
        let Some(binding) = binding else { continue };
        let (part, cable) = legacy_part(id, role, binding);
        parts.push(part);
        cables.extend(cable);
    }
    (parts, cables)
}

/// A file as the routes serve it; `version` is the hash of the bytes it was
/// read from.
pub fn bench_from_file(slug: &str, file: BenchFile, version: Option<String>) -> Bench {
    let (parts, cables, legacy) = match file.parts {
        Some(parts) => (parts, file.cables, false),
        None => {
            let (parts, mut cables) = legacy_layout(file.dut.as_ref(), file.dev_bench.as_ref());
            cables.extend(file.cables);
            (parts, cables, true)
        }
    };
    Bench {
        slug: slug.to_string(),
        name: file.name,
        legacy,
        version,
        parts,
        cables,
        configs: file.configs,
        signals: file.signals,
    }
}

/// A bench as it is saved: the new shape, and never the old keys.
pub fn file_from_bench(bench: &Bench, saved_at_utc_ms: u64) -> BenchFile {
    BenchFile {
        name: bench.name.trim().to_string(),
        saved_at_utc_ms,
        dut: None,
        dev_bench: None,
        parts: Some(bench.parts.clone()),
        cables: bench.cables.clone(),
        configs: bench.configs.clone(),
        signals: bench.signals.clone(),
    }
}

/// The probe serial a part is reached by: its own, else the one on the far
/// end of an SWD lead from it — a debug probe cabled to the board — in the
/// bench-level cables or any config (fix round 1: a role whose probe is a
/// separate pod was proposed nothing).
///
/// **Leads that reach different probes in different configs are not
/// guessed between** (fix round 2): `Err` names them, and nothing is
/// proposed — which config is live is phase 4's record, not this file's.
fn reached_probe(part: &Part, parts: &[Part], cables: &[Cable], configs: &[Config], templates: &Templates) -> Result<Option<String>, String> {
    if let Some(own) = part.probe_serial() {
        return Ok(Some(own.to_string()));
    }
    let swd = |p: &Part, connector: &str| {
        templates
            .get(&p.template)
            .and_then(|t| t.connector(connector))
            .is_some_and(|c| c.kind == Kind::Swd)
    };
    let scopes = std::iter::once(("the bench level".to_string(), cables.iter().map(|c| (c.from.as_str(), c.to.as_str())).collect::<Vec<_>>()))
        .chain(configs.iter().map(|cfg| (format!("config “{}”", cfg.name), cfg.cables.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect())));
    // serial → where it was reached, in first-seen order
    let mut reached: Vec<(String, Vec<String>)> = Vec::new();
    for (scope, pairs) in scopes {
        for (a, b) in pairs {
            for (near, far) in [(a, b), (b, a)] {
                let (Some((np, nc)), Some((fp, fc))) = (near.split_once('.'), far.split_once('.')) else { continue };
                if np != part.id || !swd(part, nc) {
                    continue;
                }
                let Some(other) = parts.iter().find(|p| p.id == fp) else { continue };
                if !swd(other, fc) {
                    continue;
                }
                let Some(serial) = other.probe_serial() else { continue };
                match reached.iter_mut().find(|(s, _)| s == serial) {
                    Some((_, wheres)) if !wheres.contains(&scope) => wheres.push(scope.clone()),
                    Some(_) => {}
                    None => reached.push((serial.to_string(), vec![scope.clone()])),
                }
            }
        }
    }
    match reached.as_slice() {
        [] => Ok(None),
        [(serial, _)] => Ok(Some(serial.clone())),
        several => Err(format!(
            "its SWD leads reach different probes ({})",
            several
                .iter()
                .map(|(serial, wheres)| format!("{serial} in {}", wheres.join(" and ")))
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

/// A role's binding, derived from the part holding it, for the topology
/// routes that still read `dut`/`dev-bench`.
///
/// **The chip is the one the part recorded, with the board it was recorded
/// for** — a fallback only: the apply route looks the chip up the way the
/// role picker does and uses this when nothing answers and the role still
/// holds that board (decision 48).
fn role_binding(file: &BenchFile, parts: &[Part], role: Role, templates: &Templates) -> Option<RoleBinding> {
    let part = parts.iter().find(|p| p.role == Some(role))?;
    let (probe_serial, probe_note) = match reached_probe(part, parts, &file.cables, &file.configs, templates) {
        Ok(serial) => (serial.unwrap_or_default(), None),
        Err(note) => (String::new(), Some(note)),
    };
    let mut binding = RoleBinding {
        board: part.board.clone().unwrap_or_default(),
        chip: part.chip.as_ref().map(|c| c.chip.clone()).unwrap_or_default(),
        chip_board: part.chip.as_ref().map(|c| c.board.clone()),
        probe_serial,
        probe_note,
        ..RoleBinding::default()
    };
    if role == Role::DevBench {
        let vcom = part.usb.values().find_map(|f| f.get(&Function::Vcom));
        (binding.link_port_serial, binding.link_port_interface) = match vcom {
            Some(Binding::Interface(i)) => (None, Some(*i)),
            Some(Binding::Serial(s)) => (Some(s.clone()), None),
            Some(Binding::Port(p)) => (p.serial.clone(), p.interface),
            None => (None, None),
        };
    }
    Some(binding)
}

/// A bench file as a topology profile: the old keys for a legacy file, and
/// the role parts' bindings for one in the new shape.
pub fn profile_from_file(file: BenchFile, templates: &Templates) -> Profile {
    match &file.parts {
        Some(parts) => Profile {
            dut: role_binding(&file, parts, Role::Dut, templates),
            dev_bench: role_binding(&file, parts, Role::DevBench, templates),
            name: file.name.clone(),
            saved_at_utc_ms: file.saved_at_utc_ms,
            signals: file.signals.clone(),
        },
        None => Profile {
            name: file.name,
            saved_at_utc_ms: file.saved_at_utc_ms,
            dut: file.dut,
            dev_bench: file.dev_bench,
            signals: file.signals,
        },
    }
}

// ---- reading and writing ----------------------------------------------------

/// Why a bench file could not be read: three different facts.
#[derive(Debug)]
pub enum LoadError {
    Missing,
    Unreadable(String),
    Unparseable(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Missing => write!(f, "no such bench"),
            LoadError::Unreadable(e) | LoadError::Unparseable(e) => write!(f, "{e}"),
        }
    }
}

/// The relative path a bench file is reported under.
pub fn bench_file(slug: &str) -> String {
    format!("embarch/topologies/{slug}.toml")
}

/// A bench slug is what [`slugify`] makes of itself; anything else names no
/// bench (fix round 1: a path was slugified, so `Bench-A` read `bench-a`).
pub fn is_slug(slug: &str) -> bool {
    !slug.is_empty() && slugify(slug) == slug
}

/// A bench file and its version.
pub fn read_file(repo: &Path, slug: &str) -> Result<(BenchFile, String), LoadError> {
    let path = profile_path(repo, slug);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(e) => return Err(LoadError::Unreadable(format!("could not read {}: {e}", bench_file(slug)))),
    };
    let version = version_of(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| LoadError::Unparseable(format!("{} is not UTF-8 text", bench_file(slug))))?;
    let file = toml::from_str(&text)
        .map_err(|e| LoadError::Unparseable(format!("{}: {}", bench_file(slug), super::toml_error_line(&text, &e))))?;
    Ok((file, version))
}

pub fn load(repo: &Path, slug: &str) -> Result<Bench, LoadError> {
    read_file(repo, slug).map(|(file, version)| bench_from_file(slug, file, Some(version)))
}

/// One row of `GET /api/bench/benches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BenchRow {
    pub slug: String,
    pub name: String,
    pub legacy: bool,
}

/// Every bench file, newest first; **one that will not load is listed in
/// `errors`**, never skipped — and so is a directory that exists and cannot
/// be read, an entry that cannot be read, a name that is not UTF-8, and a
/// name that is not a slug (no route could address it).
pub fn list(repo: &Path) -> (Vec<BenchRow>, Vec<FileError>) {
    let dir = topologies_dir(repo);
    let mut rows: Vec<(u64, BenchRow)> = Vec::new();
    let mut errors = Vec::new();
    let here = "embarch/topologies".to_string();
    match dir.try_exists() {
        Ok(true) => {}
        Ok(false) => return (Vec::new(), errors),
        Err(e) => {
            errors.push(FileError { file: here, error: format!("could not tell whether the directory exists: {e}") });
            return (Vec::new(), errors);
        }
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(FileError { file: here, error: format!("could not read the directory: {e}") });
            return (Vec::new(), errors);
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                errors.push(FileError { file: here.clone(), error: format!("could not read a directory entry: {e}") });
                continue;
            }
        };
        if path.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let shown = format!("embarch/topologies/{}", path.file_name().unwrap_or_default().to_string_lossy());
        let Some(slug) = path.file_stem().and_then(|x| x.to_str()).map(str::to_string) else {
            errors.push(FileError { file: shown, error: "the file name is not UTF-8, so no slug names it".to_string() });
            continue;
        };
        if !is_slug(&slug) {
            errors.push(FileError {
                file: shown,
                error: format!("`{slug}` is not a slug, so no route can address it; rename the file to {}.toml", slugify(&slug)),
            });
            continue;
        }
        match read_file(repo, &slug) {
            Ok((file, _)) => rows.push((
                file.saved_at_utc_ms,
                BenchRow { legacy: file.parts.is_none(), name: file.name, slug },
            )),
            Err(e) => errors.push(FileError { file: bench_file(&slug), error: e.to_string() }),
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug.cmp(&b.1.slug)));
    errors.sort_by(|a, b| a.file.cmp(&b.file));
    (rows.into_iter().map(|(_, r)| r).collect(), errors)
}

/// Whether the old Topology save or delete may touch `<slug>.toml`: only
/// when nothing is there, or the file parses as TOML and holds none of the
/// bench editor's keys. **A file that does not parse is refused** — it may
/// be a bench the editor wrote with one typo, and writing the old shape over
/// it would drop every part, cable and config (fix round 1).
pub fn guard_old_shape(repo: &Path, slug: &str) -> Result<(), String> {
    let path = profile_path(repo, slug);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{} could not be read ({e}), so it is left alone", bench_file(slug))),
    };
    let table: toml::Table = text.parse().map_err(|_| {
        format!(
            "{} does not parse, and it may be a bench the editor saved; it is left alone — fix it by \
             hand, or use another name",
            bench_file(slug)
        )
    })?;
    if ["parts", "cables", "configs"].iter().any(|k| table.contains_key(*k)) {
        return Err(format!(
            "the saved bench '{slug}' has parts, cables or configs, which the Topology tab's save and \
             delete would drop — use another name, or edit it in the bench editor"
        ));
    }
    Ok(())
}

/// The most parts and the most cables in one scope (the bench level, or one
/// config) a bench may hold: far past any bench, and a bound on what one
/// check request costs.
pub const MAX_PARTS: usize = 500;
pub const MAX_CABLES: usize = 2000;
pub const MAX_CONFIGS: usize = 100;
/// The most USB bindings (function → identity) on one part.
pub const MAX_BINDINGS: usize = 32;

/// What a save refuses: only what would make the file unreadable as a bench
/// or ambiguous to address. Everything else — a cable of the wrong kind, a
/// full connector — is the checks' business, and a bench that breaks one
/// still saves, so a legacy file with a problem can be rewritten.
pub fn check_shape(bench: &Bench, templates: &Templates) -> Result<(), String> {
    check_size(bench)?;
    // A `usb` key is a connector of the part: one its template declares, or
    // — with the template missing — at least a slug. Anything else names no
    // connector and is refused before it reaches a file (fix round 3).
    for part in &bench.parts {
        let declared = templates.get(&part.template);
        for connector in part.usb.keys() {
            let ok = match declared {
                Some(t) => t.connector(connector).is_some(),
                None => !connector.is_empty() && slugify(connector) == *connector,
            };
            if !ok {
                return Err(format!(
                    "part `{}` binds USB functions on `{}`, which is not a connector of `{}`",
                    part.id,
                    connector.escape_debug(),
                    part.template
                ));
            }
        }
    }
    if slugify(&bench.name).is_empty() {
        return Err("a bench needs a name with at least one letter or digit in it".to_string());
    }
    let mut ids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for part in &bench.parts {
        if part.id.is_empty() || part.id.contains('.') || part.id.chars().any(char::is_whitespace) {
            return Err(format!(
                "part id `{}` must be non-empty, with no `.` and no space: a cable end is `<part>.<connector>`",
                part.id
            ));
        }
        if !ids.insert(&part.id) {
            return Err(format!("two parts have the id `{}`", part.id));
        }
    }
    let mut names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for config in &bench.configs {
        if config.name.trim().is_empty() {
            return Err("a config needs a name".to_string());
        }
        if !names.insert(&config.name) {
            return Err(format!("two configs are named `{}`", config.name));
        }
    }
    Ok(())
}

/// The size bounds alone, for the routes that take a bench without saving it.
pub fn check_size(bench: &Bench) -> Result<(), String> {
    if bench.parts.len() > MAX_PARTS {
        return Err(format!("a bench holds at most {MAX_PARTS} parts; this one has {}", bench.parts.len()));
    }
    let longest = bench.configs.iter().map(|c| c.cables.len()).chain([bench.cables.len()]).max().unwrap_or(0);
    if longest > MAX_CABLES {
        return Err(format!("a bench or config holds at most {MAX_CABLES} cables; one here has {longest}"));
    }
    if bench.configs.len() > MAX_CONFIGS {
        return Err(format!("a bench holds at most {MAX_CONFIGS} configs; this one has {}", bench.configs.len()));
    }
    if let Some(p) = bench.parts.iter().find(|p| p.usb.values().map(BTreeMap::len).sum::<usize>() > MAX_BINDINGS) {
        return Err(format!("a part binds at most {MAX_BINDINGS} USB functions; `{}` binds more", p.id));
    }
    Ok(())
}

/// The lists after the parts, as the file writes them; each left out when
/// empty.
#[derive(Serialize)]
struct ListsOut<'a> {
    #[serde(skip_serializing_if = "<[Cable]>::is_empty")]
    cables: &'a [Cable],
    #[serde(skip_serializing_if = "<[Config]>::is_empty")]
    configs: &'a [Config],
    #[serde(skip_serializing_if = "<[SignalLink]>::is_empty")]
    signals: &'a [SignalLink],
}

/// One value as inline TOML: `"x"`, `[20, 110]`, `{ board = "…", chip = "…" }`.
fn inline<T: Serialize + ?Sized>(value: &T) -> anyhow::Result<String> {
    let mut out = String::new();
    value.serialize(toml::ser::ValueSerializer::new(&mut out))?;
    Ok(out)
}

/// The file's TOML, in reading order. A legacy file's old keys are never
/// written: a file is only ever saved in the new shape.
///
/// **Each part is written by hand, key by key, as inline values**, so a
/// recorded chip reads `chip = { board = "…", chip = "…" }` and a part's USB
/// bindings stay on its own lines instead of trailing it as `[parts.usb.…]`
/// tables; the defaults a hand-written file leaves out (`flip = false`, an
/// empty `usb`, every `None`) are left out. `parts` is always written, even
/// empty, because its presence is what makes the file the new shape.
pub fn bench_toml(file: &BenchFile) -> anyhow::Result<String> {
    fn key(out: &mut String, k: &str, v: String) {
        out.push_str(k);
        out.push_str(" = ");
        out.push_str(&v);
        out.push('\n');
    }
    let mut out = String::new();
    key(&mut out, "name", inline(&file.name)?);
    key(&mut out, "saved_at_utc_ms", inline(&file.saved_at_utc_ms)?);
    let parts = file.parts.as_deref().unwrap_or_default();
    if parts.is_empty() {
        key(&mut out, "parts", "[]".to_string());
    }
    for p in parts {
        out.push_str("\n[[parts]]\n");
        key(&mut out, "id", inline(&p.id)?);
        key(&mut out, "template", inline(&p.template)?);
        if let Some(label) = &p.label {
            key(&mut out, "label", inline(label)?);
        }
        if let Some(at) = &p.at {
            key(&mut out, "at", inline(at)?);
        }
        if let Some(role) = &p.role {
            key(&mut out, "role", inline(role)?);
        }
        if let Some(base) = &p.mounted_on {
            key(&mut out, "mounted_on", inline(base)?);
        }
        if p.flip {
            key(&mut out, "flip", "true".to_string());
        }
        if let Some(board) = &p.board {
            key(&mut out, "board", inline(board)?);
        }
        if let Some(chip) = &p.chip {
            key(&mut out, "chip", inline(chip)?);
        }
        // The whole map as one inline table: the value serializer quotes
        // every key it holds correctly, which a key spliced into
        // `usb.<connector>` by hand did not (fix round 3).
        if !p.usb.is_empty() {
            key(&mut out, "usb", inline(&p.usb)?);
        }
    }
    let lists = toml::to_string(&ListsOut { cables: &file.cables, configs: &file.configs, signals: &file.signals })?;
    if !lists.is_empty() {
        out.push('\n');
        out.push_str(&lists);
    }
    Ok(out)
}

/// Writes a bench file through the repo writer (`super::write_in_repo`):
/// atomically, never through a link, never outside the repo; `Create` fails
/// on anything already at the path.
///
/// **The bytes are read back before they replace anything** (fix round 3):
/// a writer bug that produced text no load could read, or text that reads
/// back as a different bench, would otherwise overwrite a good file with one
/// unreadable from then on. A mismatch leaves the old file untouched.
pub fn write(repo: &Path, slug: &str, file: &BenchFile, mode: WriteMode) -> Result<(), WriteError> {
    let text = bench_toml(file).map_err(|e| WriteError::Other(format!("{e:#}")))?;
    super::round_trips(&text, file).map_err(|e| WriteError::Other(format!("{}: {e}", bench_file(slug))))?;
    write_in_repo(repo, &["embarch", "topologies"], &format!("{slug}.toml"), text.as_bytes(), mode).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The file `api_save_profile` writes today.
    const LEGACY: &str = r#"
name = "BLE bench"
saved_at_utc_ms = 1755000000000

[dut]
board = "wearable-rev6"
chip = "nRF54L15"
probe_serial = "001057729826"

[dev-bench]
board = "nrf54l15dk/nrf54l15/cpuapp"
chip = "nRF54L15"
probe_serial = "001050288460"
link_port_serial = "D607104"
link_port_interface = 2

[[signals]]
name = "outpost"
origin_role = "dut"
direction = "dut-to-host"

[signals.route]
kind = "direct"
port_serial = "FT9ABCDE"
"#;

    #[test]
    fn a_legacy_file_loads_as_the_ble_layout() {
        let file: BenchFile = toml::from_str(LEGACY).unwrap();
        let bench = bench_from_file("ble-bench", file, None);
        assert!(bench.legacy);
        let ids: Vec<_> = bench.parts.iter().map(|p| (p.id.as_str(), p.template.as_str())).collect();
        assert_eq!(ids, [("host", "this-machine"), ("dev-bench", "nrf54l15dk"), ("dut", "generic-board")]);
        let part = |id: &str| bench.parts.iter().find(|p| p.id == id).unwrap();
        let dev = part("dev-bench");
        assert_eq!(dev.role, Some(Role::DevBench));
        assert_eq!(dev.board.as_deref(), Some("nrf54l15dk/nrf54l15/cpuapp"));
        assert_eq!(dev.usb["usb"][&Function::Probe], Binding::Serial("001050288460".into()));
        assert_eq!(
            dev.usb["usb"][&Function::Vcom],
            Binding::Port(PortBinding { serial: Some("D607104".into()), interface: Some(2), ..Default::default() })
        );
        assert_eq!(part("dut").probe_serial(), Some("001057729826"));
        assert_eq!(
            dev.chip,
            Some(RecordedChip { board: "nrf54l15dk/nrf54l15/cpuapp".into(), chip: "nRF54L15".into() }),
            "the recorded chip is kept as the fallback, with the board it was recorded for"
        );
        assert_eq!(
            bench.cables,
            [
                Cable { from: "dev-bench.usb".into(), to: "host".into() },
                Cable { from: "dut.usb".into(), to: "host".into() },
            ]
        );
        assert!(bench.configs.is_empty());
        assert_eq!(bench.signals.len(), 1);
    }

    #[test]
    fn a_saved_bench_writes_the_new_shape_and_never_the_old_keys() {
        let file: BenchFile = toml::from_str(LEGACY).unwrap();
        let bench = bench_from_file("ble-bench", file, None);
        let text = bench_toml(&file_from_bench(&bench, 1)).unwrap();
        assert!(!text.contains("[dut]") && !text.contains("[dev-bench]"), "{text}");
        assert!(!text.contains("flip"), "{text}");
        assert!(text.contains("chip = { board = \"wearable-rev6\", chip = \"nRF54L15\" }"), "{text}");
        assert!(text.contains("usb = { usb = { probe = \"001050288460\""), "{text}");
        assert!(!text.contains("[parts."), "a part's values stay on its own lines: {text}");
        let back = bench_from_file("ble-bench", toml::from_str(&text).unwrap(), None);
        assert!(!back.legacy);
        assert_eq!(back.parts, bench.parts);
        assert_eq!(back.cables, bench.cables);
        assert_eq!(back.signals, bench.signals);
    }

    #[test]
    fn an_empty_bench_saves_as_the_new_shape_and_stays_one() {
        let bench = Bench { slug: String::new(), name: "Empty".into(), legacy: false, version: None, parts: vec![], cables: vec![], configs: vec![], signals: vec![] };
        let text = bench_toml(&file_from_bench(&bench, 1)).unwrap();
        assert!(text.contains("parts = []"), "{text}");
        assert!(!bench_from_file("empty", toml::from_str(&text).unwrap(), None).legacy);
    }

    #[test]
    fn the_topology_routes_read_role_bindings_from_either_shape() {
        let t = templates();
        let legacy: BenchFile = toml::from_str(LEGACY).unwrap();
        let old = profile_from_file(legacy.clone(), &t);
        let new = profile_from_file(toml::from_str(&bench_toml(&file_from_bench(&bench_from_file("b", legacy, None), 1)).unwrap()).unwrap(), &t);
        let (o, n) = (old.dev_bench.unwrap(), new.dev_bench.unwrap());
        assert_eq!((o.board, o.probe_serial, o.link_port_serial, o.link_port_interface), (n.board, n.probe_serial, n.link_port_serial, n.link_port_interface));
        assert_eq!(n.chip, o.chip, "the recorded chip survives the conversion");
        assert_eq!(old.dut.unwrap().probe_serial, new.dut.unwrap().probe_serial);
        assert_eq!(new.signals.len(), 1);
    }

    #[test]
    fn the_contract_example_round_trips_through_json_and_toml() {
        let example = json!({
            "slug": "pd-bench-drp", "name": "PD bench · DRP", "legacy": false, "version": "0123456789abcdef-42",
            "parts": [
                { "id": "host", "template": "this-machine", "label": null, "at": [20, 150], "role": null,
                  "mounted_on": null, "flip": false, "board": null, "chip": null, "usb": {} },
                { "id": "partner", "template": "nucleo-g0b1re", "label": "pd-bench", "at": [300, 330], "role": null,
                  "mounted_on": null, "flip": false, "board": null, "chip": null,
                  "usb": { "stlink": { "probe": "0671FF3833554B3043164817", "vcom": 1 } } },
                { "id": "drp", "template": "x-nucleo-drp1m1", "label": null, "at": null, "role": null,
                  "mounted_on": "partner", "flip": false, "board": null, "chip": null, "usb": {} },
                { "id": "dut", "template": "generic-board", "label": "DUT", "at": [900, 150], "role": "dut",
                  "mounted_on": null, "flip": false, "board": "nrf54l15dk/nrf54l15/cpuapp", "chip": null, "usb": {} }
            ],
            "cables": [ { "from": "partner.stlink", "to": "host" } ],
            "configs": [ { "name": "A · DUT sources", "cables": [["dut.usb-c-1", "cy4500.in"], ["cy4500.out", "drp.usb-c"]] } ],
            "signals": [ ]
        });
        let bench: Bench = serde_json::from_value(example.clone()).unwrap();
        assert_eq!(serde_json::to_value(&bench).unwrap(), example, "JSON is the contract's, field for field");
        let text = bench_toml(&file_from_bench(&bench, 1)).unwrap();
        let back = bench_from_file("pd-bench-drp", toml::from_str(&text).unwrap(), Some("0123456789abcdef-42".into()));
        assert_eq!(serde_json::to_value(&back).unwrap(), example, "{text}");
    }

    #[test]
    fn a_dragged_position_rounds_to_whole_units() {
        let part: Part = serde_json::from_value(json!({ "id": "a", "template": "cy4500", "at": [20.6, 149.4] })).unwrap();
        assert_eq!(part.at, Some([21, 149]));
    }

    #[test]
    fn a_save_refuses_only_what_makes_the_file_ambiguous() {
        let part = |id: &str| Part { id: id.into(), template: "cy4500".into(), label: None, at: None, role: None, mounted_on: None, flip: false, board: None, chip: None, usb: BTreeMap::new() };
        let mut bench = Bench { slug: String::new(), name: "B".into(), legacy: false, version: None, parts: vec![part("a"), part("b")], cables: vec![], configs: vec![], signals: vec![] };
        assert!(check_shape(&bench, &templates()).is_ok());
        bench.parts.push(part("a"));
        assert!(check_shape(&bench, &templates()).unwrap_err().contains("two parts"));
        bench.parts[2] = part("x.y");
        assert!(check_shape(&bench, &templates()).unwrap_err().contains("no `.`"));
        bench.parts.pop();
        bench.name = "///".into();
        assert!(check_shape(&bench, &templates()).unwrap_err().contains("name"));
        bench.name = "B".into();
        bench.parts = (0..=MAX_PARTS).map(|i| part(&format!("p{i}"))).collect();
        assert!(check_shape(&bench, &templates()).unwrap_err().contains("at most 500 parts"));
        bench.parts = vec![part("a")];
        bench.configs = (0..=MAX_CONFIGS).map(|i| Config { name: format!("c{i}"), cables: vec![] }).collect();
        assert!(check_shape(&bench, &templates()).unwrap_err().contains("at most 100 configs"));
        bench.configs.clear();
        bench.parts[0].usb = (0..=MAX_BINDINGS)
            .map(|i| (format!("u{i}"), BTreeMap::from([(Function::Probe, Binding::Serial(format!("s{i}")))])))
            .collect();
        assert!(check_shape(&bench, &templates()).unwrap_err().contains("at most 32 USB functions"));
    }

    fn templates() -> Templates {
        builtins().into_iter().map(|t| (t.id.clone(), t)).collect()
    }

    #[test]
    fn a_role_whose_probe_is_a_separate_pod_is_reached_over_its_swd_lead() {
        let file: BenchFile = toml::from_str(
            r#"
name = "pod"
[[parts]]
id = "host"
template = "this-machine"
[[parts]]
id = "probe"
template = "debug-probe"
usb.usb = { probe = "J1" }
[[parts]]
id = "dut"
template = "generic-board"
role = "dut"
board = "plank"
[[parts]]
id = "swd-dut"
template = "pd-dut"
role = "dev-bench"
[[cables]]
from = "probe.usb"
to = "host"
[[configs]]
name = "A"
cables = [["probe.swd", "swd-dut.swd"]]
"#,
        )
        .unwrap();
        let mut t = templates();
        let pd: crate::bench::PartTemplate = toml::from_str(
            "id = \"pd-dut\"\nlabel = \"PD DUT\"\ncategory = \"board\"\nlook = \"power-board\"\n[[connectors]]\nid = \"swd\"\nlabel = \"SWD\"\nkind = \"swd\"\nside = \"left\"\n",
        )
        .unwrap();
        t.insert(pd.id.clone(), pd);
        let mut two = file.clone();
        let profile = profile_from_file(file, &t);
        assert_eq!(profile.dev_bench.unwrap().probe_serial, "J1", "reached over the SWD lead in config A");
        let dut = profile.dut.unwrap();
        assert_eq!((dut.probe_serial.as_str(), dut.probe_note.as_deref()), ("", None), "no probe on its part and no lead");

        // A second config leading to another probe is not guessed between.
        let parts = two.parts.as_mut().unwrap();
        parts.push(Part { id: "probe2".into(), template: "debug-probe".into(), ..parts[1].clone() });
        parts.last_mut().unwrap().usb = BTreeMap::from([("usb".into(), BTreeMap::from([(Function::Probe, Binding::Serial("J2".into()))]))]);
        two.configs.push(Config { name: "B".into(), cables: vec![["probe2.swd".into(), "swd-dut.swd".into()]] });
        let bench = profile_from_file(two, &t).dev_bench.unwrap();
        assert_eq!(bench.probe_serial, "");
        assert_eq!(bench.probe_note.as_deref(), Some("its SWD leads reach different probes (J1 in config “A”; J2 in config “B”)"));
    }

    /// Fix round 3, M1: USB keys a hand-spliced `usb.<key>` mangled — a
    /// newline, a carriage return, both quote kinds — round-trip when the map
    /// is written whole, a save refuses a key that is no connector of the
    /// part, and a write whose bytes would not read back as the same bench
    /// refuses before it replaces anything.
    #[test]
    fn hostile_usb_keys_round_trip_are_refused_on_save_and_never_reach_a_file_unread() {
        let hostile = ["a\nb", "c\rd", "q\"'q", "'''", "\"\"\"", "dot.ted", "sp ace", "ü", "tab\there"];
        let mut part: Part = serde_json::from_value(json!({ "id": "p", "template": "gone-template" })).unwrap();
        for k in hostile {
            part.usb.insert(k.to_string(), BTreeMap::from([(Function::Probe, Binding::Serial(format!("S-{k}")))]));
        }
        // (a) the whole map written at once reads back key for key.
        let file = BenchFile { name: "hostile".into(), parts: Some(vec![part.clone()]), ..BenchFile::default() };
        let text = bench_toml(&file).unwrap();
        let back: BenchFile = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(back, file, "{text}");
        assert!(crate::bench::round_trips(&text, &file).is_ok());

        // (b) a save refuses each hostile key: not declared by a known
        // template, not a slug when the template is missing.
        let bench = Bench { slug: String::new(), name: "hostile".into(), legacy: false, version: None, parts: vec![part], cables: vec![], configs: vec![], signals: vec![] };
        let e = check_shape(&bench, &templates()).unwrap_err();
        assert!(e.contains("is not a connector of `gone-template`"), "{e}");
        let mut known: Part = serde_json::from_value(json!({ "id": "dk", "template": "nrf54l15dk" })).unwrap();
        known.usb.insert("usb".into(), BTreeMap::from([(Function::Probe, Binding::Serial("J".into()))]));
        assert!(check_shape(&Bench { parts: vec![known.clone()], ..bench.clone() }, &templates()).is_ok());
        known.usb.insert("radio2".into(), BTreeMap::new());
        assert!(check_shape(&Bench { parts: vec![known], ..bench }, &templates()).is_err(), "undeclared, even though a slug");

        // (c) bytes that would not read back as the bench are refused before
        // the write, and the good file stays as it was.
        let dir = repo("roundtrip");
        std::fs::write(profile_path(&dir, "hostile"), LEGACY).unwrap();
        let other = BenchFile { name: "something else".into(), ..file.clone() };
        assert!(crate::bench::round_trips(&text, &other).unwrap_err().contains("does not read back"));
        assert!(crate::bench::round_trips::<BenchFile>("parts = [", &file).unwrap_err().contains("does not parse back"));
        assert_eq!(std::fs::read_to_string(profile_path(&dir, "hostile")).unwrap(), LEGACY);
        write(&dir, "hostile", &file, WriteMode::Replace).unwrap();
        assert_eq!(read_file(&dir, "hostile").unwrap().0, file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recorded_chip_travels_with_its_board() {
        let legacy: BenchFile = toml::from_str(LEGACY).unwrap();
        let mut bench = bench_from_file("b", legacy, None);
        let dut = bench.parts.iter_mut().find(|p| p.id == "dut").unwrap();
        dut.board = Some("plank".into());
        let file: BenchFile = toml::from_str(&bench_toml(&file_from_bench(&bench, 1)).unwrap()).unwrap();
        let binding = profile_from_file(file, &templates()).dut.unwrap();
        assert_eq!(binding.board, "plank");
        assert_eq!(binding.chip, "nRF54L15");
        assert_eq!(binding.chip_board.as_deref(), Some("wearable-rev6"), "apply sees it was for another board");
    }

    fn repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ebm-files-{tag}-{}-{}", std::process::id(), crate::topology::now_utc_ms()));
        std::fs::create_dir_all(topologies_dir(&dir)).unwrap();
        dir
    }

    #[test]
    fn the_old_save_touches_only_a_missing_or_old_shape_file() {
        let dir = repo("guard");
        let at = |slug: &str| profile_path(&dir, slug);
        assert!(guard_old_shape(&dir, "none").is_ok());
        std::fs::write(at("old"), LEGACY).unwrap();
        assert!(guard_old_shape(&dir, "old").is_ok());
        std::fs::write(at("new"), "name = \"n\"\nparts = []\n").unwrap();
        assert!(guard_old_shape(&dir, "new").unwrap_err().contains("parts, cables or configs"));
        std::fs::write(at("typo"), "name = \"n\"\n[[parts]\nid = \"a\"\n").unwrap();
        assert!(guard_old_shape(&dir, "typo").unwrap_err().contains("does not parse"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_no_slug_names_is_listed_as_an_error_and_a_version_tracks_the_bytes() {
        let dir = repo("slugs");
        std::fs::write(profile_path(&dir, "Bench-A"), LEGACY).unwrap();
        std::fs::write(profile_path(&dir, "ok"), LEGACY).unwrap();
        let (rows, errors) = list(&dir);
        assert_eq!(rows.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(), ["ok"]);
        assert!(errors[0].error.contains("rename the file to bench-a.toml"), "{errors:?}");
        assert!(!is_slug("Bench-A") && !is_slug("") && is_slug("bench-a"));
        let v1 = load(&dir, "ok").unwrap().version.unwrap();
        std::fs::write(profile_path(&dir, "ok"), format!("{LEGACY}\n")).unwrap();
        assert_ne!(load(&dir, "ok").unwrap().version.unwrap(), v1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
