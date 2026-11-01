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

use super::{builtins, FileError, Function, Kind, Role, GENERIC_BOARD, HUB_TEMPLATE};
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
    pub fn part(&self, id: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.id == id)
    }

    pub fn config(&self, name: &str) -> Option<&Config> {
        self.configs.iter().find(|c| c.name == name)
    }
}

/// A bench file as TOML holds it: today's shape, and the old keys a legacy
/// file still carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// A file as the routes serve it.
pub fn bench_from_file(slug: &str, file: BenchFile) -> Bench {
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

/// A role's binding, derived from the part holding it, for the topology
/// routes that still read `dut`/`dev-bench`.
///
/// **The chip is left empty**: a part carries none, because a chip is looked
/// up from the board type, never typed (decision 48). The apply route looks
/// it up the same way the role picker does.
fn role_binding(parts: &[Part], role: Role) -> Option<RoleBinding> {
    let part = parts.iter().find(|p| p.role == Some(role))?;
    let mut binding = RoleBinding {
        board: part.board.clone().unwrap_or_default(),
        probe_serial: part.probe_serial().unwrap_or_default().to_string(),
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
pub fn profile_from_file(file: BenchFile) -> Profile {
    match &file.parts {
        Some(parts) => Profile {
            dut: role_binding(parts, Role::Dut),
            dev_bench: role_binding(parts, Role::DevBench),
            name: file.name,
            saved_at_utc_ms: file.saved_at_utc_ms,
            signals: file.signals,
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

pub fn read_file(repo: &Path, slug: &str) -> Result<BenchFile, LoadError> {
    let path = profile_path(repo, slug);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(e) => return Err(LoadError::Unreadable(format!("could not read {}: {e}", bench_file(slug)))),
    };
    toml::from_str(&text)
        .map_err(|e| LoadError::Unparseable(format!("{}: {}", bench_file(slug), super::toml_error_line(&text, &e))))
}

pub fn load(repo: &Path, slug: &str) -> Result<Bench, LoadError> {
    read_file(repo, slug).map(|file| bench_from_file(slug, file))
}

/// One row of `GET /api/bench/benches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BenchRow {
    pub slug: String,
    pub name: String,
    pub legacy: bool,
}

/// Every bench file, newest first; **one that will not load is listed in
/// `errors`**, never skipped, and so is a directory that exists and cannot be
/// read.
pub fn list(repo: &Path) -> (Vec<BenchRow>, Vec<FileError>) {
    let dir = topologies_dir(repo);
    let mut rows: Vec<(u64, BenchRow)> = Vec::new();
    let mut errors = Vec::new();
    if !dir.exists() {
        return (Vec::new(), errors);
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(FileError { file: "embarch/topologies".to_string(), error: format!("could not read the directory: {e}") });
            return (Vec::new(), errors);
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|x| x.to_str()).map(str::to_string) else { continue };
        match read_file(repo, &slug) {
            Ok(file) => rows.push((
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

/// What a save refuses: only what would make the file unreadable as a bench
/// or ambiguous to address. Everything else — a cable of the wrong kind, a
/// full connector — is the checks' business, and a bench that breaks one
/// still saves, so a legacy file with a problem can be rewritten.
pub fn check_shape(bench: &Bench) -> Result<(), String> {
    if slugify(&bench.name).is_empty() {
        return Err("a bench needs a name with at least one letter or digit in it".to_string());
    }
    let mut ids: Vec<&str> = Vec::new();
    for part in &bench.parts {
        if part.id.is_empty() || part.id.contains('.') || part.id.chars().any(char::is_whitespace) {
            return Err(format!(
                "part id `{}` must be non-empty, with no `.` and no space: a cable end is `<part>.<connector>`",
                part.id
            ));
        }
        if ids.contains(&part.id.as_str()) {
            return Err(format!("two parts have the id `{}`", part.id));
        }
        ids.push(&part.id);
    }
    let mut names: Vec<&str> = Vec::new();
    for config in &bench.configs {
        if config.name.trim().is_empty() {
            return Err("a config needs a name".to_string());
        }
        if names.contains(&config.name.as_str()) {
            return Err(format!("two configs are named `{}`", config.name));
        }
        names.push(&config.name);
    }
    Ok(())
}

/// A part as the file writes it: the JSON's fields in the same order, less
/// the defaults a hand-written file would leave out (`flip = false`, an empty
/// `usb`, every `None`).
#[derive(Serialize)]
struct PartOut<'a> {
    id: &'a str,
    template: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: &'a Option<[i32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: &'a Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mounted_on: &'a Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    flip: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    board: &'a Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    usb: &'a BTreeMap<String, BTreeMap<Function, Binding>>,
}

/// The new shape as the file writes it, in reading order. `parts` is always
/// written, even empty, because its presence is what makes the file the new
/// shape; the other lists are left out when empty.
#[derive(Serialize)]
struct BenchFileOut<'a> {
    name: &'a str,
    saved_at_utc_ms: u64,
    parts: Vec<PartOut<'a>>,
    #[serde(skip_serializing_if = "<[Cable]>::is_empty")]
    cables: &'a [Cable],
    #[serde(skip_serializing_if = "<[Config]>::is_empty")]
    configs: &'a [Config],
    #[serde(skip_serializing_if = "<[SignalLink]>::is_empty")]
    signals: &'a [SignalLink],
}

/// The file's TOML. A legacy file's old keys are never written: a file is
/// only ever saved in the new shape.
pub fn bench_toml(file: &BenchFile) -> anyhow::Result<String> {
    let parts = file.parts.as_deref().unwrap_or_default();
    let out = BenchFileOut {
        name: &file.name,
        saved_at_utc_ms: file.saved_at_utc_ms,
        parts: parts
            .iter()
            .map(|p| PartOut {
                id: &p.id,
                template: &p.template,
                label: &p.label,
                at: &p.at,
                role: &p.role,
                mounted_on: &p.mounted_on,
                flip: p.flip,
                board: &p.board,
                usb: &p.usb,
            })
            .collect(),
        cables: &file.cables,
        configs: &file.configs,
        signals: &file.signals,
    };
    Ok(toml::to_string(&out)?)
}

/// Writes through a temporary file and a rename, so a reader never sees half
/// a bench.
pub fn write(repo: &Path, slug: &str, file: &BenchFile) -> anyhow::Result<()> {
    let dir = topologies_dir(repo);
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("failed to create {}: {e}", dir.display()))?;
    let text = bench_toml(file)?;
    let path = profile_path(repo, slug);
    let tmp = dir.join(format!(".{slug}.toml.tmp"));
    std::fs::write(&tmp, text).map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
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
        let bench = bench_from_file("ble-bench", file);
        assert!(bench.legacy);
        let ids: Vec<_> = bench.parts.iter().map(|p| (p.id.as_str(), p.template.as_str())).collect();
        assert_eq!(ids, [("host", "this-machine"), ("dev-bench", "nrf54l15dk"), ("dut", "generic-board")]);
        let dev = bench.part("dev-bench").unwrap();
        assert_eq!(dev.role, Some(Role::DevBench));
        assert_eq!(dev.board.as_deref(), Some("nrf54l15dk/nrf54l15/cpuapp"));
        assert_eq!(dev.usb["usb"][&Function::Probe], Binding::Serial("001050288460".into()));
        assert_eq!(
            dev.usb["usb"][&Function::Vcom],
            Binding::Port(PortBinding { serial: Some("D607104".into()), interface: Some(2), ..Default::default() })
        );
        assert_eq!(bench.part("dut").unwrap().probe_serial(), Some("001057729826"));
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
        let bench = bench_from_file("ble-bench", file);
        let text = bench_toml(&file_from_bench(&bench, 1)).unwrap();
        assert!(!text.contains("[dut]") && !text.contains("[dev-bench]"), "{text}");
        assert!(!text.contains("flip"), "{text}");
        assert!(!text.contains("chip"), "{text}");
        let back = bench_from_file("ble-bench", toml::from_str(&text).unwrap());
        assert!(!back.legacy);
        assert_eq!(back.parts, bench.parts);
        assert_eq!(back.cables, bench.cables);
        assert_eq!(back.signals, bench.signals);
    }

    #[test]
    fn an_empty_bench_saves_as_the_new_shape_and_stays_one() {
        let bench = Bench { slug: String::new(), name: "Empty".into(), legacy: false, parts: vec![], cables: vec![], configs: vec![], signals: vec![] };
        let text = bench_toml(&file_from_bench(&bench, 1)).unwrap();
        assert!(text.contains("parts = []"), "{text}");
        assert!(!bench_from_file("empty", toml::from_str(&text).unwrap()).legacy);
    }

    #[test]
    fn the_topology_routes_read_role_bindings_from_either_shape() {
        let legacy: BenchFile = toml::from_str(LEGACY).unwrap();
        let old = profile_from_file(legacy.clone());
        let new = profile_from_file(toml::from_str(&bench_toml(&file_from_bench(&bench_from_file("b", legacy), 1)).unwrap()).unwrap());
        let (o, n) = (old.dev_bench.unwrap(), new.dev_bench.unwrap());
        assert_eq!((o.board, o.probe_serial, o.link_port_serial, o.link_port_interface), (n.board, n.probe_serial, n.link_port_serial, n.link_port_interface));
        assert_eq!(n.chip, "", "a part carries no chip; apply looks it up");
        assert_eq!(old.dut.unwrap().probe_serial, new.dut.unwrap().probe_serial);
        assert_eq!(new.signals.len(), 1);
    }

    #[test]
    fn the_contract_example_round_trips_through_json_and_toml() {
        let example = json!({
            "slug": "pd-bench-drp", "name": "PD bench · DRP", "legacy": false,
            "parts": [
                { "id": "host", "template": "this-machine", "label": null, "at": [20, 150], "role": null,
                  "mounted_on": null, "flip": false, "board": null, "usb": {} },
                { "id": "partner", "template": "nucleo-g0b1re", "label": "pd-bench", "at": [300, 330], "role": null,
                  "mounted_on": null, "flip": false, "board": null,
                  "usb": { "stlink": { "probe": "0671FF3833554B3043164817", "vcom": 1 } } },
                { "id": "drp", "template": "x-nucleo-drp1m1", "label": null, "at": null, "role": null,
                  "mounted_on": "partner", "flip": false, "board": null, "usb": {} },
                { "id": "dut", "template": "generic-board", "label": "DUT", "at": [900, 150], "role": "dut",
                  "mounted_on": null, "flip": false, "board": "nrf54l15dk/nrf54l15/cpuapp", "usb": {} }
            ],
            "cables": [ { "from": "partner.stlink", "to": "host" } ],
            "configs": [ { "name": "A · DUT sources", "cables": [["dut.usb-c-1", "cy4500.in"], ["cy4500.out", "drp.usb-c"]] } ],
            "signals": [ ]
        });
        let bench: Bench = serde_json::from_value(example.clone()).unwrap();
        assert_eq!(serde_json::to_value(&bench).unwrap(), example, "JSON is the contract's, field for field");
        let text = bench_toml(&file_from_bench(&bench, 1)).unwrap();
        let back = bench_from_file("pd-bench-drp", toml::from_str(&text).unwrap());
        assert_eq!(serde_json::to_value(&back).unwrap(), example, "{text}");
    }

    #[test]
    fn a_dragged_position_rounds_to_whole_units() {
        let part: Part = serde_json::from_value(json!({ "id": "a", "template": "cy4500", "at": [20.6, 149.4] })).unwrap();
        assert_eq!(part.at, Some([21, 149]));
    }

    #[test]
    fn a_save_refuses_only_what_makes_the_file_ambiguous() {
        let part = |id: &str| Part { id: id.into(), template: "cy4500".into(), label: None, at: None, role: None, mounted_on: None, flip: false, board: None, usb: BTreeMap::new() };
        let mut bench = Bench { slug: String::new(), name: "B".into(), legacy: false, parts: vec![part("a"), part("b")], cables: vec![], configs: vec![], signals: vec![] };
        assert!(check_shape(&bench).is_ok());
        bench.parts.push(part("a"));
        assert!(check_shape(&bench).unwrap_err().contains("two parts"));
        bench.parts[2] = part("x.y");
        assert!(check_shape(&bench).unwrap_err().contains("no `.`"));
        bench.parts.pop();
        bench.name = "///".into();
        assert!(check_shape(&bench).unwrap_err().contains("name"));
    }
}
