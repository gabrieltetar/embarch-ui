//! The bench editor's model: part templates, the vocabulary they are written
//! in, and the benches built from them (`embarch-doc`'s
//! `embarch-bench-editor-proposal.md`, phase 1).
//!
//! # Three layers
//!
//! A **part template** is a kind of device — a NUCLEO-G0B1RE, a PD analyzer, a
//! bench supply — with the connectors a human would cable. A **bench** places
//! templates as parts, binds their `usb-host` functions to USB identities, and
//! joins their connectors with cables: those that end at this machine are
//! bench-level, every other one belongs to a named **config**. The
//! **vocabulary** is what both are written in, and it is served from here
//! rather than restated in the browser (spec.md: "a vocabulary is served,
//! never restated in `app.js`").
//!
//! **Nothing in this module reads or writes hardware, and nothing calls
//! Core.** A bench is a description; the checks in [`check`] read the
//! description, never the bench in front of you.

pub mod check;
pub mod files;
pub mod live;
pub mod routes;
#[cfg(test)]
mod drive_fixture;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

/// One closed word list: an enum whose serde spelling is the word itself, and
/// the full list of words in order, so the vocabulary route serves exactly
/// what a file is parsed against. A word outside the list fails the parse,
/// which is what lists a repo template that uses one in `errors`.
macro_rules! vocabulary {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }
    };
}

vocabulary! {
    /// What a connector physically is, which decides what it joins.
    Kind {
        UsbC = "usb-c",
        UsbHost = "usb-host",
        Banana = "banana",
        Terminal = "terminal",
        Swd = "swd",
        PinHeader = "pin-header",
        Radio = "radio",
        StackSocket = "stack-socket",
        StackPlug = "stack-plug",
    }
}

vocabulary! {
    /// Which edge of a part a connector is drawn on.
    Side {
        Left = "left",
        Right = "right",
        Top = "top",
    }
}

vocabulary! {
    /// What a power connector does on its bus.
    Power {
        Source = "source",
        Sink = "sink",
        Drp = "drp",
        Supply = "supply",
        Load = "load",
        In = "in",
        Out = "out",
        Pass = "pass",
    }
}

vocabulary! {
    /// What a part is, for the parts bin.
    Category {
        Computer = "computer",
        Board = "board",
        Shield = "shield",
        Instrument = "instrument",
        Analyzer = "analyzer",
        Probe = "probe",
        Adapter = "adapter",
    }
}

vocabulary! {
    /// Which drawing a part gets: the thing you would look for on a shelf.
    Look {
        Monitor = "monitor",
        Nucleo64 = "nucleo64",
        Shield = "shield",
        Devkit = "devkit",
        PowerBoard = "power-board",
        Instrument = "instrument",
        Analyzer = "analyzer",
        Pod = "pod",
        Breakout = "breakout",
        Board = "board",
    }
}

vocabulary! {
    /// What a `usb-host` connector carries to this machine.
    Function {
        Probe = "probe",
        Vcom = "vcom",
        Scpi = "scpi",
        BootloadApp = "bootload-app",
        BootloadBootloader = "bootload-bootloader",
    }
}

vocabulary! {
    /// The two roles (decision 44), the same pair `topology::ROLES` holds.
    Role {
        Dut = "dut",
        DevBench = "dev-bench",
    }
}

/// The kinds a cable may join, each pair once. A `usb-host` joins a hub part,
/// never another connector ([`HUB_KIND`]); a stack socket and plug are never
/// cabled — a shield joins its board through `mounted_on`.
pub const JOINS: &[(Kind, Kind)] = &[
    (Kind::UsbC, Kind::UsbC),
    (Kind::Banana, Kind::Banana),
    (Kind::Banana, Kind::Terminal),
    (Kind::Terminal, Kind::Terminal),
    (Kind::Swd, Kind::Swd),
    (Kind::PinHeader, Kind::PinHeader),
    (Kind::Radio, Kind::Radio),
];

/// The kind that joins a hub, and only a hub.
pub const HUB_KIND: Kind = Kind::UsbHost;

/// Whether a cable may join a connector of kind `a` to one of kind `b`.
pub fn joins(a: Kind, b: Kind) -> bool {
    JOINS.iter().any(|&(x, y)| (x == a && y == b) || (x == b && y == a))
}

/// The kinds that carry power, and so make up a bus: cables and passthrough
/// pairs join them into one, and only a connector of one takes a power role.
pub fn carries_power(kind: Kind) -> bool {
    matches!(kind, Kind::UsbC | Kind::Banana | Kind::Terminal)
}

impl Function {
    /// A serial port's functions, which may name the USB interface they are
    /// on; a probe and an instrument line take none.
    pub fn takes_interface(self) -> bool {
        matches!(self, Function::Vcom | Function::BootloadApp | Function::BootloadBootloader)
    }

    /// An instrument line, the one function with line facts (baud,
    /// terminators, pacing, acks: `embarch-topology` decision 49).
    pub fn takes_line_facts(self) -> bool {
        self == Function::Scpi
    }
}

/// What a power role means on a bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerClass {
    Provider,
    Consumer,
    Either,
    Pass,
}

impl Power {
    pub fn class(self) -> PowerClass {
        match self {
            Power::Source | Power::Supply | Power::Out => PowerClass::Provider,
            Power::Sink | Power::Load | Power::In => PowerClass::Consumer,
            Power::Drp => PowerClass::Either,
            Power::Pass => PowerClass::Pass,
        }
    }
}

fn powers_of(class: PowerClass) -> Vec<&'static str> {
    Power::ALL.iter().filter(|p| p.class() == class).map(|p| p.as_str()).collect()
}

/// `GET /api/bench/vocabulary`'s body.
///
/// Besides the word lists, the rules between fields that a part editor has
/// to follow to offer only what a save accepts: which kinds take a power
/// role (`power_kinds`), and which functions take an interface or line facts
/// — the same predicates [`PartTemplate::check`] applies — and under
/// `template`, the limits a template file is held to.
pub fn vocabulary_json() -> serde_json::Value {
    fn words<T: Copy>(all: &[T], text: fn(T) -> &'static str) -> Vec<&'static str> {
        all.iter().map(|&w| text(w)).collect()
    }
    fn functions_where(keep: fn(Function) -> bool) -> Vec<&'static str> {
        Function::ALL.iter().copied().filter(|&f| keep(f)).map(Function::as_str).collect()
    }
    json!({
        "kinds": words(Kind::ALL, Kind::as_str),
        "power_kinds": Kind::ALL.iter().copied().filter(|&k| carries_power(k)).map(Kind::as_str).collect::<Vec<_>>(),
        "sides": words(Side::ALL, Side::as_str),
        "joins": JOINS.iter().map(|(a, b)| [a.as_str(), b.as_str()]).collect::<Vec<_>>(),
        "hub_kind": HUB_KIND.as_str(),
        "power": {
            "providers": powers_of(PowerClass::Provider),
            "consumers": powers_of(PowerClass::Consumer),
            "either": powers_of(PowerClass::Either),
            "pass": powers_of(PowerClass::Pass),
        },
        "categories": words(Category::ALL, Category::as_str),
        "looks": words(Look::ALL, Look::as_str),
        "functions": words(Function::ALL, Function::as_str),
        // What a part's `role` may hold, so the editor's role picker offers
        // the parser's own words (phase 3).
        "roles": words(Role::ALL, Role::as_str),
        "interface_functions": functions_where(Function::takes_interface),
        "line_functions": functions_where(Function::takes_line_facts),
        "template": template_limits(),
    })
}

// ---- part templates ---------------------------------------------------------

/// One function a `usb-host` connector carries, with the facts that function
/// needs: a `vcom`'s USB interface when the template knows it, an `scpi`
/// instrument's default line facts (`embarch-topology` decision 49 — defaults
/// only, a declaration may override each).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionEntry {
    pub function: Function,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baud: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_terminator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_terminator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_interval_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_ok: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_err: Option<String>,
}

/// One line fact an instrument line takes: its TOML name, and whether it is a
/// number (else text).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LineFact {
    pub name: &'static str,
    pub numeric: bool,
}

/// The line facts, in the order a template writes them: served, so the part
/// editor builds its fields from this list rather than its own copy.
/// [`FunctionEntry::has_line_facts`] destructures every field, so a fact added
/// to the struct does not compile until it is placed here or there.
pub const LINE_FACTS: &[LineFact] = &[
    LineFact { name: "baud", numeric: true },
    LineFact { name: "write_terminator", numeric: false },
    LineFact { name: "read_terminator", numeric: false },
    LineFact { name: "min_interval_ms", numeric: true },
    LineFact { name: "ack_ok", numeric: false },
    LineFact { name: "ack_err", numeric: false },
];

impl FunctionEntry {
    /// Whether any line fact ([`LINE_FACTS`]) is set.
    fn has_line_facts(&self) -> bool {
        let FunctionEntry { function: _, interface: _, baud, write_terminator, read_terminator, min_interval_ms, ack_ok, ack_err } = self;
        baud.is_some()
            || write_terminator.is_some()
            || read_terminator.is_some()
            || min_interval_ms.is_some()
            || ack_ok.is_some()
            || ack_err.is_some()
    }

    fn check(&self, connector: &str) -> Result<(), String> {
        if self.interface.is_some() && !self.function.takes_interface() {
            return Err(format!(
                "connector `{connector}`: `{}` takes no interface — only a serial port does",
                self.function.as_str()
            ));
        }
        if self.has_line_facts() && !self.function.takes_line_facts() {
            return Err(format!(
                "connector `{connector}`: line facts belong to `scpi`, not `{}`",
                self.function.as_str()
            ));
        }
        Ok(())
    }
}

fn one() -> u32 {
    1
}

/// One connector on a part template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connector {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    pub side: Side,
    #[serde(default)]
    pub power: Option<Power>,
    #[serde(default = "one")]
    pub max_cables: u32,
    /// What a `usb-host` connector carries; empty on every other kind, and
    /// empty on a `usb-host` that declares nothing (the CY4500's own link).
    #[serde(default)]
    pub functions: Vec<FunctionEntry>,
}

impl Connector {
    pub fn carries(&self, function: Function) -> bool {
        self.functions.iter().any(|f| f.function == function)
    }
}

/// The most connectors one template may list: far past any part, and a bound
/// on what a repo file can make every check cost.
pub const MAX_CONNECTORS: usize = 64;

/// The fewest cables a connector may take: one that takes none is no connector.
pub const MIN_MAX_CABLES: u32 = 1;

fn range<T: Into<u64>>(min: T, max: T) -> serde_json::Value {
    json!({ "min": min.into(), "max": max.into() })
}

/// What a template file may hold, served beside the vocabulary so the part
/// editor restates no limit (spec.md: "a limit enforced server-side is
/// served"): the connector cap, every number field's range — its integer
/// type's, narrowed where the check narrows it — and the line facts.
fn template_limits() -> serde_json::Value {
    json!({
        "max_connectors": MAX_CONNECTORS,
        "numbers": {
            "width": range(u32::MIN, u32::MAX),
            "min_height": range(u32::MIN, u32::MAX),
            "rows_top": range(u32::MIN, u32::MAX),
            "max_cables": range(MIN_MAX_CABLES, u32::MAX),
            "interface": range(u8::MIN, u8::MAX),
            "baud": range(u32::MIN, u32::MAX),
            "min_interval_ms": range(u32::MIN, u32::MAX),
        },
        "line_facts": LINE_FACTS,
    })
}

/// The width a template gets when it states none: the generic board's.
fn default_width() -> u32 {
    180
}

/// A kind of device, as `embarch/parts/<id>.toml` or a built-in holds it.
///
/// **A template carries no serial** (decision 44): which unit is on the bench
/// is the bench's binding, never the template's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartTemplate {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    pub category: Category,
    pub look: Look,
    /// This machine: it has no ports of its own, and a cable reaching it is
    /// a port named by what it carries.
    #[serde(default)]
    pub hub: bool,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default)]
    pub min_height: Option<u32>,
    #[serde(default)]
    pub rows_top: Option<u32>,
    /// Pairs of connectors that are one bus (the CY4500's two USB-C).
    #[serde(default)]
    pub passthrough: Vec<[String; 2]>,
    #[serde(default)]
    pub connectors: Vec<Connector>,
}

impl PartTemplate {
    pub fn connector(&self, id: &str) -> Option<&Connector> {
        self.connectors.iter().find(|c| c.id == id)
    }

    /// What a parse cannot catch: the rules between fields.
    pub fn check(&self) -> Result<(), String> {
        if self.id.is_empty() || crate::topology::slugify(&self.id) != self.id {
            return Err(format!(
                "id `{}` is not a slug: lowercase letters, digits and single hyphens, because it \
                 names the file embarch/parts/<id>.toml",
                self.id
            ));
        }
        if self.label.trim().is_empty() {
            return Err(format!("part `{}` has no label", self.id));
        }
        if self.connectors.len() > MAX_CONNECTORS {
            return Err(format!("part `{}` has {} connectors; a template holds at most {MAX_CONNECTORS}", self.id, self.connectors.len()));
        }
        if self.hub && !self.connectors.is_empty() {
            return Err(format!(
                "part `{}` is a hub, and a hub has no connectors of its own: every cable that \
                 reaches it is a port",
                self.id
            ));
        }
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for c in &self.connectors {
            if c.id.is_empty() || c.id.contains('.') || c.id.chars().any(char::is_whitespace) {
                return Err(format!(
                    "connector id `{}` must be non-empty, with no `.` and no space: a cable end \
                     is `<part>.<connector>`",
                    c.id
                ));
            }
            if !seen.insert(&c.id) {
                return Err(format!("connector `{}` is listed twice", c.id));
            }
            if c.max_cables < MIN_MAX_CABLES {
                return Err(format!("connector `{}` has max_cables {}; it takes at least {MIN_MAX_CABLES}", c.id, c.max_cables));
            }
            if c.power.is_some() && !carries_power(c.kind) {
                return Err(format!(
                    "connector `{}` is `{}`, which carries no power, so it takes no power role",
                    c.id,
                    c.kind.as_str()
                ));
            }
            // A function is what a connector carries to this machine, so only
            // the kind that joins the hub carries one.
            if !c.functions.is_empty() && c.kind != HUB_KIND {
                return Err(format!(
                    "connector `{}` is `{}`; only a `{}` carries functions",
                    c.id,
                    c.kind.as_str(),
                    HUB_KIND.as_str()
                ));
            }
            for f in &c.functions {
                f.check(&c.id)?;
            }
        }
        for [a, b] in &self.passthrough {
            if a == b {
                return Err(format!("passthrough pairs `{a}` with itself"));
            }
            for end in [a, b] {
                match self.connector(end) {
                    None => return Err(format!("passthrough names `{end}`, which is not a connector here")),
                    Some(c) if !carries_power(c.kind) => {
                        return Err(format!(
                            "passthrough names `{end}`, a `{}`: only usb-c, banana and terminal \
                             connectors make up a bus",
                            c.kind.as_str()
                        ))
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }
}

/// The built-in templates, one TOML file each, embedded in the binary.
const BUILTINS: &[(&str, &str)] = &[
    ("this-machine", include_str!("builtin/this-machine.toml")),
    ("nucleo-g0b1re", include_str!("builtin/nucleo-g0b1re.toml")),
    ("x-nucleo-drp1m1", include_str!("builtin/x-nucleo-drp1m1.toml")),
    ("x-nucleo-snk1m1", include_str!("builtin/x-nucleo-snk1m1.toml")),
    ("cy4500", include_str!("builtin/cy4500.toml")),
    ("nrf54l15dk", include_str!("builtin/nrf54l15dk.toml")),
    ("debug-probe", include_str!("builtin/debug-probe.toml")),
    ("usb-uart-bridge", include_str!("builtin/usb-uart-bridge.toml")),
    ("usb-c-breakout", include_str!("builtin/usb-c-breakout.toml")),
    ("bench-supply", include_str!("builtin/bench-supply.toml")),
    ("scpi-load", include_str!("builtin/scpi-load.toml")),
    ("generic-board", include_str!("builtin/generic-board.toml")),
];

/// The built-in template ids a legacy bench is laid out from.
pub const HUB_TEMPLATE: &str = "this-machine";
pub const GENERIC_BOARD: &str = "generic-board";

/// The built-ins, parsed. A built-in that does not parse is a build defect,
/// which `every_builtin_parses_and_checks` catches before it ships; here it
/// is skipped rather than taking the route down.
pub fn builtins() -> Vec<PartTemplate> {
    BUILTINS
        .iter()
        .filter_map(|(_, text)| parse_template(text).ok())
        .collect()
}

/// Where a served template came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Builtin,
    Repo,
}

/// A template as `GET /api/bench/parts` serves it.
#[derive(Debug, Clone, Serialize)]
pub struct ServedPart {
    #[serde(flatten)]
    pub template: PartTemplate,
    pub source: Source,
    /// A repo template's file version, which `PUT /api/bench/parts/{id}`
    /// must name; `None` for a built-in, which cannot be written.
    pub version: Option<String>,
}

/// A file that could not be used, and why — listed, never dropped.
///
/// When the error is about one template or one bench, it also names it —
/// the template's `id` (the file stem) or the bench's `slug` — so the page
/// can say "unreadable" on the part or bench that uses it instead of
/// "missing". A directory that could not be read names neither.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileError {
    pub file: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

/// Every template a bench may use: the built-ins, then the open repo's.
#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub parts: Vec<ServedPart>,
    pub errors: Vec<FileError>,
}

impl Catalog {
    /// The templates by id, for the checks.
    pub fn by_id(&self) -> BTreeMap<String, PartTemplate> {
        self.parts.iter().map(|p| (p.template.id.clone(), p.template.clone())).collect()
    }
}

/// A TOML parse error as one line: where, then what. toml's own message
/// carries a multi-line excerpt, which reads badly in a list.
pub fn toml_error_line(text: &str, err: &toml::de::Error) -> String {
    match err.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: {}", err.message().trim())
        }
        None => err.message().trim().to_string(),
    }
}

/// Parses and checks one template file's text.
pub fn parse_template(text: &str) -> Result<PartTemplate, String> {
    let template: PartTemplate = toml::from_str(text).map_err(|e| toml_error_line(text, &e))?;
    template.check()?;
    Ok(template)
}

pub fn parts_dir(repo: &Path) -> std::path::PathBuf {
    repo.join("embarch").join("parts")
}

/// The relative path a repo template is reported under.
fn repo_file(id: &str) -> String {
    format!("embarch/parts/{id}.toml")
}

/// The built-ins and, when a repo is open, its `embarch/parts/*.toml`.
///
/// **A file that does not parse is listed in `errors`, never dropped**, and
/// so is one whose id is not its file name, and one that reuses a built-in's
/// id (owner, 2026-11-01): a repo cannot redefine the CY4500 every other repo
/// draws. A directory that exists and cannot be read is an error too —
/// "could not read" is not "no templates".
pub fn catalog(repo: Option<&Path>) -> Catalog {
    let builtins = builtins();
    let mut parts: Vec<ServedPart> = builtins
        .iter()
        .cloned()
        .map(|template| ServedPart { template, source: Source::Builtin, version: None })
        .collect();
    let mut errors = Vec::new();
    let Some(repo) = repo else {
        return Catalog { parts, errors };
    };
    let dir = parts_dir(repo);
    match dir.try_exists() {
        Ok(true) => {}
        Ok(false) => return Catalog { parts, errors },
        Err(e) => {
            errors.push(FileError { file: "embarch/parts".to_string(), error: format!("could not tell whether the directory exists: {e}"), ..Default::default() });
            return Catalog { parts, errors };
        }
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(FileError { file: "embarch/parts".to_string(), error: format!("could not read the directory: {e}"), ..Default::default() });
            return Catalog { parts, errors };
        }
    };
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) == Some("toml") {
                    paths.push(path);
                }
            }
            Err(e) => errors.push(FileError { file: "embarch/parts".to_string(), error: format!("could not read a directory entry: {e}"), ..Default::default() }),
        }
    }
    paths.sort();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
            errors.push(FileError {
                file: format!("embarch/parts/{}", path.file_name().unwrap_or_default().to_string_lossy()),
                error: "the file name is not UTF-8, so it cannot be a part id".to_string(),
                ..Default::default()
            });
            continue;
        };
        let file = repo_file(&stem);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                errors.push(FileError { file, error: format!("could not read: {e}"), id: Some(stem), slug: None });
                continue;
            }
        };
        let text = match String::from_utf8(bytes.clone()) {
            Ok(t) => t,
            Err(_) => {
                errors.push(FileError { file, error: "the file is not UTF-8 text".to_string(), id: Some(stem), slug: None });
                continue;
            }
        };
        let template = match parse_template(&text) {
            Ok(t) => t,
            Err(error) => {
                errors.push(FileError { file, error, id: Some(stem), slug: None });
                continue;
            }
        };
        if template.id != stem {
            errors.push(FileError {
                file,
                error: format!("id `{}` is not this file's name; a template lives at embarch/parts/<id>.toml", template.id),
                id: Some(stem),
                slug: None,
            });
            continue;
        }
        if let Some(builtin) = builtins.iter().find(|b| b.id == template.id) {
            errors.push(FileError {
                file,
                error: builtin_clash(&template.id, &builtin.label),
                id: Some(stem),
                slug: None,
            });
            continue;
        }
        parts.push(ServedPart { template, source: Source::Repo, version: Some(version_of(&bytes)) });
    }
    Catalog { parts, errors }
}

/// The refusal for a repo template reusing a built-in's id, naming both.
pub fn builtin_clash(id: &str, builtin_label: &str) -> String {
    format!(
        "id `{id}` is the built-in part `{id}` ({builtin_label}); a repo template cannot reuse a \
         built-in's id — give embarch/parts/{id}.toml another id"
    )
}

/// A file's version: an FNV-1a hash of its bytes, which a save names to say
/// "I am replacing the file I read". Not a security measure — a concurrency
/// token, stable across processes and builds, so a page held over a restart
/// still saves.
pub fn version_of(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = bytes.iter().fold(OFFSET, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME));
    format!("{hash:016x}-{}", bytes.len())
}

/// Whether `text` — about to be written — parses back as exactly `meant`.
/// Every repo write checks this **before** it replaces anything, so a
/// serializer that produced text no load could read, or that reads back as
/// something else, leaves the old file as it was (fix round 3).
pub fn round_trips<T>(text: &str, meant: &T) -> Result<(), String>
where
    T: serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let back: T = toml::from_str(text).map_err(|e| {
        format!("the text about to be written does not parse back ({}), so nothing was written", toml_error_line(text, &e))
    })?;
    if back == *meant {
        Ok(())
    } else {
        Err("the text about to be written does not read back as what was saved, so nothing was written".to_string())
    }
}

/// Whether a repo write may replace a file or must create one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Create,
    Replace,
}

#[derive(Debug)]
pub enum WriteError {
    /// `Create` found something already at the path — a file, a directory or
    /// a link.
    Exists,
    Other(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Exists => write!(f, "something already exists at that path"),
            WriteError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// Every file this UI writes into a repo goes through here: `bytes` to
/// `<repo>/<dirs…>/<file_name>`, atomically, and **never outside the repo**.
///
/// A repo is someone else's checkout, so a link committed at
/// `embarch/parts/x.toml -> ../../../.bashrc` must not get `~/.bashrc`
/// rewritten. So the directory is resolved and must lie inside the resolved
/// repo (checked before it is created, so no directory is made outside
/// either); the bytes go to a fresh temporary file opened with `create_new`,
/// which never follows a link; and the temporary file is renamed over the
/// target, which replaces a link rather than writing through it. `Create`
/// links the temporary file into place instead, which fails on anything
/// already there.
pub fn write_in_repo(repo: &Path, dirs: &[&str], file_name: &str, bytes: &[u8], mode: WriteMode) -> Result<std::path::PathBuf, WriteError> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let other = |what: &str, e: std::io::Error| WriteError::Other(format!("{what}: {e}"));
    let (root, dir) = repo_dir(repo, dirs, file_name)?;
    let mut existing = dir.as_path();
    while !existing.try_exists().map_err(|e| other(&format!("checking {}", existing.display()), e))? {
        existing = existing.parent().unwrap_or(repo);
    }
    inside(&root, existing)?;
    std::fs::create_dir_all(&dir).map_err(|e| other(&format!("creating {}", dir.display()), e))?;
    inside(&root, &dir)?;

    let target = dir.join(file_name);
    let tmp = dir.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(other(&format!("writing {}", tmp.display()), e));
    }
    let placed = match mode {
        WriteMode::Replace => std::fs::rename(&tmp, &target),
        WriteMode::Create => std::fs::hard_link(&tmp, &target),
    };
    let _ = std::fs::remove_file(&tmp);
    match placed {
        Ok(()) => Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(WriteError::Exists),
        Err(e) => Err(other(&format!("writing {}", target.display()), e)),
    }
}

/// The resolved repo root and the (unresolved) directory `dirs` names in it,
/// after refusing a file name that is not plain.
fn repo_dir(repo: &Path, dirs: &[&str], file_name: &str) -> Result<(std::path::PathBuf, std::path::PathBuf), WriteError> {
    if file_name.is_empty() || file_name.contains(['/', '\\']) || file_name.starts_with('.') {
        return Err(WriteError::Other(format!("`{file_name}` is not a plain file name")));
    }
    let root = repo
        .canonicalize()
        .map_err(|e| WriteError::Other(format!("the repo {}: {e}", repo.display())))?;
    Ok((root, dirs.iter().fold(repo.to_path_buf(), |p, d| p.join(d))))
}

/// Refuses a path that resolves outside the resolved repo root.
fn inside(root: &Path, p: &Path) -> Result<(), WriteError> {
    let resolved = p
        .canonicalize()
        .map_err(|e| WriteError::Other(format!("resolving {}: {e}", p.display())))?;
    if resolved.starts_with(root) {
        Ok(())
    } else {
        Err(WriteError::Other(format!(
            "{} resolves outside the repo, to {}; nothing was changed",
            p.display(),
            resolved.display()
        )))
    }
}

/// Removes `<repo>/<dirs…>/<file_name>` with [`write_in_repo`]'s rule: the
/// directory must resolve inside the repo, so a linked `embarch/topologies`
/// cannot get a file outside the checkout deleted. A link at the file itself
/// is removed as a link, never followed.
pub fn remove_in_repo(repo: &Path, dirs: &[&str], file_name: &str) -> Result<(), WriteError> {
    let (root, dir) = repo_dir(repo, dirs, file_name)?;
    inside(&root, &dir)?;
    std::fs::remove_file(dir.join(file_name))
        .map_err(|e| WriteError::Other(format!("removing {}: {e}", dir.join(file_name).display())))
}

/// Held across every check-then-write of a repo file — the bench editor's
/// saves and the old Topology save and delete alike — so no write in this
/// process lands between another's check and its write.
static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes [`WRITES`]. A poisoned lock is taken anyway: it guards no data, only
/// an ordering, and a panic mid-write left nothing to protect.
pub(crate) fn hold_writes() -> std::sync::MutexGuard<'static, ()> {
    WRITES.lock().unwrap_or_else(|e| e.into_inner())
}

/// A connector as a repo template's file writes it: no empty `functions`,
/// no `None`.
#[derive(Serialize)]
struct ConnectorOut<'a> {
    id: &'a str,
    label: &'a str,
    kind: Kind,
    side: Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    power: Option<Power>,
    max_cables: u32,
    #[serde(skip_serializing_if = "<[FunctionEntry]>::is_empty")]
    functions: &'a [FunctionEntry],
}

/// A template as its file writes it, in the order a human reads one.
#[derive(Serialize)]
struct TemplateOut<'a> {
    id: &'a str,
    label: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle: &'a Option<String>,
    category: Category,
    look: Look,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    hub: bool,
    width: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows_top: Option<u32>,
    #[serde(skip_serializing_if = "<[[String; 2]]>::is_empty")]
    passthrough: &'a [[String; 2]],
    connectors: Vec<ConnectorOut<'a>>,
}

/// Writes a repo template as TOML, **refusing text that would not read back
/// as the same template** ([`round_trips`]).
pub fn template_toml(t: &PartTemplate) -> anyhow::Result<String> {
    let text = template_toml_unchecked(t)?;
    round_trips(&text, t).map_err(|e| anyhow::anyhow!("embarch/parts/{}.toml: {e}", t.id))?;
    Ok(text)
}

fn template_toml_unchecked(t: &PartTemplate) -> anyhow::Result<String> {
    let out = TemplateOut {
        id: &t.id,
        label: &t.label,
        subtitle: &t.subtitle,
        category: t.category,
        look: t.look,
        hub: t.hub,
        width: t.width,
        min_height: t.min_height,
        rows_top: t.rows_top,
        passthrough: &t.passthrough,
        connectors: t
            .connectors
            .iter()
            .map(|c| ConnectorOut {
                id: &c.id,
                label: &c.label,
                kind: c.kind,
                side: c.side,
                power: c.power,
                max_cables: c.max_cables,
                functions: &c.functions,
            })
            .collect(),
    };
    Ok(toml::to_string(&out)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses_and_checks_and_is_named_by_its_file() {
        for (stem, text) in BUILTINS {
            let template = parse_template(text).unwrap_or_else(|e| panic!("{stem}: {e}"));
            assert_eq!(template.id, *stem);
        }
        assert_eq!(builtins().len(), BUILTINS.len());
        let hubs: Vec<_> = builtins().into_iter().filter(|t| t.hub).map(|t| t.id).collect();
        assert_eq!(hubs, [HUB_TEMPLATE]);
    }

    #[test]
    fn the_builtins_carry_the_connector_facts_the_docs_state() {
        let all = builtins();
        let get = |id: &str| all.iter().find(|t| t.id == id).unwrap().clone();
        let dk = get("nrf54l15dk");
        let usb = dk.connector("usb").unwrap();
        assert!(usb.carries(Function::Probe));
        let vcom = usb.functions.iter().find(|f| f.function == Function::Vcom).unwrap();
        assert_eq!(vcom.interface, Some(2), "zephyr,console is VCOM1 at interface 2");
        let cy = get("cy4500");
        assert_eq!(cy.passthrough, [["in".to_string(), "out".to_string()]]);
        assert!(cy.connector("usb").unwrap().functions.is_empty(), "the analyzer's link declares nothing");
        let drp = get("x-nucleo-drp1m1");
        assert_eq!(drp.connector("cn3").unwrap().power, Some(Power::In));
        assert_eq!(drp.connector("cn2").unwrap().power, Some(Power::Out));
        assert_eq!(drp.connector("usb-c").unwrap().power, Some(Power::Drp));
        let load = get("scpi-load");
        let scpi = &load.connector("usb").unwrap().functions[0];
        assert_eq!(scpi.baud, Some(9600));
        assert_eq!(scpi.write_terminator.as_deref(), Some("\n"));
        assert_eq!(scpi.read_terminator.as_deref(), Some("\r\n"));
        assert_eq!(scpi.min_interval_ms, Some(200));
        assert_eq!(scpi.ack_ok.as_deref(), Some("Rexecu success"));
        assert_eq!(scpi.ack_err.as_deref(), Some("Rexecu err"));
    }

    #[test]
    fn the_vocabulary_serves_the_contracts_lists() {
        let v = vocabulary_json();
        assert_eq!(v["hub_kind"], "usb-host");
        assert_eq!(v["kinds"].as_array().unwrap().len(), 9);
        assert_eq!(v["power"]["providers"], json!(["source", "supply", "out"]));
        assert_eq!(v["power"]["consumers"], json!(["sink", "load", "in"]));
        assert_eq!(v["power"]["either"], json!(["drp"]));
        assert_eq!(v["power"]["pass"], json!(["pass"]));
        assert_eq!(v["joins"][2], json!(["banana", "terminal"]));
        assert_eq!(v["looks"][3], "devkit");
        assert_eq!(v["functions"], json!(["probe", "vcom", "scpi", "bootload-app", "bootload-bootloader"]));
        assert_eq!(v["roles"], json!(["dut", "dev-bench"]));
        assert_eq!(v["sides"], json!(["left", "right", "top"]));
        assert_eq!(v["power_kinds"], json!(["usb-c", "banana", "terminal"]));
        assert_eq!(v["interface_functions"], json!(["vcom", "bootload-app", "bootload-bootloader"]));
        assert_eq!(v["line_functions"], json!(["scpi"]));
        let t = &v["template"];
        assert_eq!(t["max_connectors"], 64);
        assert_eq!(t["numbers"]["max_cables"], json!({ "min": 1, "max": 4_294_967_295u64 }));
        assert_eq!(t["numbers"]["interface"], json!({ "min": 0, "max": 255 }));
        assert_eq!(t["numbers"]["width"], json!({ "min": 0, "max": 4_294_967_295u64 }));
        assert_eq!(
            t["line_facts"],
            json!([
                { "name": "baud", "numeric": true },
                { "name": "write_terminator", "numeric": false },
                { "name": "read_terminator", "numeric": false },
                { "name": "min_interval_ms", "numeric": true },
                { "name": "ack_ok", "numeric": false },
                { "name": "ack_err", "numeric": false },
            ])
        );
    }

    /// The fields a struct declares to serde, in order: what its derived
    /// `Deserialize` hands `deserialize_struct`, recorded by a deserializer
    /// that reads nothing else — the struct's own list, never one kept here.
    fn declared_fields<T: serde::de::DeserializeOwned>() -> Vec<&'static str> {
        #[derive(Debug)]
        struct Fields(Vec<&'static str>);
        impl std::fmt::Display for Fields {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{:?}", self.0)
            }
        }
        impl std::error::Error for Fields {}
        impl serde::de::Error for Fields {
            fn custom<T: std::fmt::Display>(_: T) -> Self {
                Fields(Vec::new())
            }
        }
        struct Recorder;
        impl<'de> serde::Deserializer<'de> for Recorder {
            type Error = Fields;
            fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Fields> {
                Err(Fields(Vec::new()))
            }
            fn deserialize_struct<V: serde::de::Visitor<'de>>(
                self,
                _: &'static str,
                fields: &'static [&'static str],
                _: V,
            ) -> Result<V::Value, Fields> {
                Err(Fields(fields.to_vec()))
            }
            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit
                unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
            }
        }
        match T::deserialize(Recorder) {
            Err(Fields(fields)) if !fields.is_empty() => fields,
            _ => panic!("{} is not a struct serde lists fields for", std::any::type_name::<T>()),
        }
    }

    /// Every field a template can hold, set: built by struct literal, so a
    /// field added to any of the three structs does not compile here until it
    /// is written in, and [`the_helper_sets_every_declared_field`] fails until
    /// it is written in with a value — then the two tests after it fail until
    /// the vocabulary serves it.
    fn every_field_set() -> PartTemplate {
        let entry = FunctionEntry {
            function: Function::Scpi,
            interface: Some(1),
            baud: Some(9600),
            write_terminator: Some("\n".into()),
            read_terminator: Some("\r\n".into()),
            min_interval_ms: Some(200),
            ack_ok: Some("ok".into()),
            ack_err: Some("err".into()),
        };
        let connector = Connector {
            id: "a".into(),
            label: "A".into(),
            kind: HUB_KIND,
            side: Side::Left,
            power: Some(Power::Drp),
            max_cables: 2,
            functions: vec![entry],
        };
        PartTemplate {
            id: "x".into(),
            label: "X".into(),
            subtitle: Some("x".into()),
            category: Category::Board,
            look: Look::Board,
            hub: false,
            width: 180,
            min_height: Some(100),
            rows_top: Some(40),
            passthrough: vec![["a".into(), "a".into()]],
            connectors: vec![connector],
        }
    }

    /// The helper leaves nothing out: each of the three objects it builds
    /// serializes exactly its struct's declared fields, none of them null or
    /// an empty list. A field left `None` is skipped (a function entry) or
    /// null (the rest), and either fails here — so the drift tests below see
    /// every field the structs have.
    #[test]
    fn the_helper_sets_every_declared_field() {
        let t = serde_json::to_value(every_field_set()).unwrap();
        let objects = [
            ("PartTemplate", &t, declared_fields::<PartTemplate>()),
            ("Connector", &t["connectors"][0], declared_fields::<Connector>()),
            ("FunctionEntry", &t["connectors"][0]["functions"][0], declared_fields::<FunctionEntry>()),
        ];
        for (name, object, declared) in objects {
            let map = object.as_object().unwrap_or_else(|| panic!("{name} is not an object"));
            let keys: std::collections::BTreeSet<&str> = map.keys().map(String::as_str).collect();
            let declared: std::collections::BTreeSet<&str> = declared.into_iter().collect();
            assert_eq!(keys, declared, "{name}: every declared field is serialized, set");
            for (k, v) in map {
                assert!(!v.is_null(), "{name}.{k} is None in every_field_set: give it a value");
                assert!(v.as_array().is_none_or(|a| !a.is_empty()), "{name}.{k} is empty in every_field_set: give it an item");
            }
        }
    }

    /// A function entry's fields are exactly `function`, `interface` and the
    /// served line facts — the struct's own declared list, so a fact the
    /// struct gains and [`LINE_FACTS`] lacks fails here, where the editor
    /// would otherwise drop it from every file it saves.
    #[test]
    fn the_served_line_facts_are_every_line_fact_field() {
        let declared: std::collections::BTreeSet<&str> = declared_fields::<FunctionEntry>().into_iter().collect();
        let mut want: std::collections::BTreeSet<&str> = ["function", "interface"].into();
        want.extend(LINE_FACTS.iter().map(|f| f.name));
        assert_eq!(declared, want);
        let t = serde_json::to_value(every_field_set()).unwrap();
        let entry = &t["connectors"][0]["functions"][0];
        for f in LINE_FACTS {
            assert_eq!(entry[f.name].is_number(), f.numeric, "{} is served as numeric = {}", f.name, f.numeric);
        }
    }

    /// The served `numbers` are exactly the template's number fields, found
    /// in a template with every field set, never listed by hand here.
    #[test]
    fn every_number_field_has_a_served_range() {
        fn numeric_keys(v: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        if v.is_number() {
                            out.insert(k.clone());
                        }
                        numeric_keys(v, out);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| numeric_keys(v, out)),
                _ => {}
            }
        }
        let mut fields = std::collections::BTreeSet::new();
        numeric_keys(&serde_json::to_value(every_field_set()).unwrap(), &mut fields);
        let served: std::collections::BTreeSet<String> =
            vocabulary_json()["template"]["numbers"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(served, fields);
        for f in LINE_FACTS.iter().filter(|f| f.numeric) {
            assert!(served.contains(f.name), "numeric line fact {} has no served range", f.name);
        }
    }

    /// Every served range is the parser's own: its ends parse, one past
    /// either end does not — so a field whose type or check changes fails
    /// here until the vocabulary says so too.
    #[test]
    fn the_served_number_ranges_are_the_parsers_own() {
        let numbers = vocabulary_json()["template"]["numbers"].clone();
        let numbers = numbers.as_object().unwrap();
        let head = "id = \"x\"\nlabel = \"X\"\ncategory = \"board\"\nlook = \"board\"\n";
        let hub = HUB_KIND.as_str();
        let with = |field: &str, n: i128| -> bool {
            let text = match field {
                "width" | "min_height" | "rows_top" => format!("{head}{field} = {n}\n"),
                "max_cables" => format!("{head}[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"swd\"\nside = \"left\"\nmax_cables = {n}\n"),
                "interface" => format!("{head}[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"{hub}\"\nside = \"left\"\nfunctions = [{{ function = \"vcom\", interface = {n} }}]\n"),
                _ => format!("{head}[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"{hub}\"\nside = \"left\"\nfunctions = [{{ function = \"scpi\", {field} = {n} }}]\n"),
            };
            parse_template(&text).is_ok()
        };
        for (field, r) in numbers {
            let (min, max) = (r["min"].as_i64().unwrap() as i128, r["max"].as_i64().unwrap() as i128);
            assert!(with(field, min) && with(field, max), "{field}: {min}..={max} should parse");
            assert!(!with(field, min - 1) && !with(field, max + 1), "{field}: past {min}..={max} should not parse");
        }
    }

    /// The editor offers a power role, functions, an interface and line facts
    /// only where the vocabulary says so; each served rule must be exactly
    /// what the check accepts, or the editor would offer a field a save
    /// refuses (or hide one it accepts).
    #[test]
    fn the_served_field_rules_are_the_checks_own() {
        let v = vocabulary_json();
        let listed = |key: &str, word: &str| v[key].as_array().unwrap().iter().any(|w| w == word);
        let base = "id = \"x\"\nlabel = \"X\"\ncategory = \"board\"\nlook = \"board\"\n";
        let connector = |kind: &str, extra: &str| {
            parse_template(&format!("{base}[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"{kind}\"\nside = \"left\"\n{extra}")).is_ok()
        };
        for kind in Kind::ALL.iter().map(|k| k.as_str()) {
            assert!(connector(kind, ""), "{kind}");
            assert_eq!(connector(kind, "power = \"drp\"\n"), listed("power_kinds", kind), "power on {kind}");
            assert_eq!(connector(kind, "functions = [{ function = \"probe\" }]\n"), kind == v["hub_kind"], "functions on {kind}");
        }
        let hub = HUB_KIND.as_str();
        let facts = v["template"]["line_facts"].as_array().unwrap().clone();
        assert_eq!(facts.len(), LINE_FACTS.len());
        for f in Function::ALL.iter().map(|f| f.as_str()) {
            let with = |extra: &str| connector(hub, &format!("functions = [{{ function = \"{f}\"{extra} }}]\n"));
            assert!(with(""), "{f}");
            assert_eq!(with(", interface = 1"), listed("interface_functions", f), "interface on {f}");
            for fact in &facts {
                let (name, numeric) = (fact["name"].as_str().unwrap(), fact["numeric"].as_bool().unwrap());
                let (right, wrong) = if numeric { ("1", "\"1\"") } else { ("\"x\"", "1") };
                assert_eq!(with(&format!(", {name} = {right}")), listed("line_functions", f), "{name} on {f}");
                assert!(!with(&format!(", {name} = {wrong}")), "{name} on {f} takes a {}", if numeric { "number" } else { "text" });
            }
        }
    }

    #[test]
    fn a_served_template_is_the_contracts_shape() {
        let cy = builtins().into_iter().find(|t| t.id == "cy4500").unwrap();
        let served = serde_json::to_value(ServedPart { template: cy, source: Source::Builtin, version: None }).unwrap();
        assert_eq!(served["version"], serde_json::Value::Null);
        assert_eq!(served["source"], "builtin");
        assert_eq!(served["hub"], false);
        assert_eq!(served["rows_top"], 72);
        assert_eq!(served["passthrough"], json!([["in", "out"]]));
        assert_eq!(
            served["connectors"][1],
            json!({ "id": "usb", "label": "USB", "kind": "usb-host", "side": "top", "power": null, "max_cables": 1, "functions": [] })
        );
        let dk = builtins().into_iter().find(|t| t.id == "nrf54l15dk").unwrap();
        let dk = serde_json::to_value(&dk).unwrap();
        assert_eq!(dk["connectors"][0]["functions"], json!([{ "function": "probe" }, { "function": "vcom", "interface": 2 }]));
        assert_eq!(dk["min_height"], serde_json::Value::Null);
    }

    fn repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ebm-parts-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(parts_dir(&dir)).unwrap();
        dir
    }

    #[test]
    fn a_repo_template_that_will_not_load_is_listed_never_dropped() {
        let dir = repo("errors");
        let p = parts_dir(&dir);
        std::fs::write(p.join("bad.toml"), "id = \"bad\"\nlabel = \"Bad\"\ncategory = \"board\"\nlook = \"board\"\n[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"usbc\"\nside = \"left\"\n").unwrap();
        std::fs::write(p.join("cy4500.toml"), "id = \"cy4500\"\nlabel = \"Mine\"\ncategory = \"analyzer\"\nlook = \"analyzer\"\n").unwrap();
        std::fs::write(p.join("named.toml"), "id = \"other\"\nlabel = \"X\"\ncategory = \"board\"\nlook = \"board\"\n").unwrap();
        std::fs::write(p.join("typo.toml"), "id = \"typo\"\nlabel = \"X\"\ncategory = \"board\"\nlook = \"board\"\nmax_cable = 2\n").unwrap();
        std::fs::write(p.join("charger.toml"), "id = \"charger\"\nlabel = \"PD charger\"\ncategory = \"board\"\nlook = \"power-board\"\n[[connectors]]\nid = \"usb-c-1\"\nlabel = \"USB-C 1\"\nkind = \"usb-c\"\nside = \"left\"\npower = \"source\"\n").unwrap();
        let cat = catalog(Some(&dir));
        let errs: BTreeMap<_, _> = cat.errors.iter().map(|e| (e.file.as_str(), e.error.as_str())).collect();
        assert!(errs["embarch/parts/bad.toml"].starts_with("line 8: unknown variant `usbc`"), "{errs:?}");
        assert!(errs["embarch/parts/cy4500.toml"].contains("built-in part `cy4500` (CY4500)"), "{errs:?}");
        assert!(errs["embarch/parts/named.toml"].contains("not this file's name"), "{errs:?}");
        assert!(errs["embarch/parts/typo.toml"].contains("max_cable"), "{errs:?}");
        assert_eq!(errs.len(), 4);
        let repo_parts: Vec<_> = cat.parts.iter().filter(|p| p.source == Source::Repo).map(|p| p.template.id.as_str()).collect();
        assert_eq!(repo_parts, ["charger"]);
        assert_eq!(cat.parts.len(), BUILTINS.len() + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_repo_and_no_parts_directory_serve_the_builtins_alone() {
        assert_eq!(catalog(None).parts.len(), BUILTINS.len());
        let dir = std::env::temp_dir().join(format!("ebm-noparts-{}", std::process::id()));
        assert!(catalog(Some(&dir)).errors.is_empty());
    }

    #[test]
    fn a_template_round_trips_through_toml_without_empty_lists() {
        let t = builtins().into_iter().find(|t| t.id == "debug-probe").unwrap();
        let text = template_toml(&t).unwrap();
        assert!(!text.contains("passthrough"), "{text}");
        assert_eq!(text.matches("functions").count(), 1, "{text}");
        assert_eq!(parse_template(&text).unwrap(), t);
    }

    #[test]
    fn rules_between_fields_are_checked() {
        let base = "id = \"x\"\nlabel = \"X\"\ncategory = \"board\"\nlook = \"board\"\n";
        let with = |extra: &str| parse_template(&format!("{base}{extra}"));
        assert!(with("[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"swd\"\nside = \"left\"\npower = \"sink\"\n").unwrap_err().contains("carries no power"));
        assert!(with("[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"swd\"\nside = \"left\"\nfunctions = [{ function = \"probe\" }]\n").unwrap_err().contains("only a `usb-host`"));
        assert!(with("[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"usb-host\"\nside = \"left\"\nfunctions = [{ function = \"probe\", interface = 1 }]\n").unwrap_err().contains("takes no interface"));
        assert!(with("[[connectors]]\nid = \"a.b\"\nlabel = \"A\"\nkind = \"swd\"\nside = \"left\"\n").unwrap_err().contains("no `.`"));
        assert!(with("passthrough = [[\"a\", \"z\"]]\n[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"usb-c\"\nside = \"left\"\n").unwrap_err().contains("`z`"));
        assert!(with("hub = true\n[[connectors]]\nid = \"a\"\nlabel = \"A\"\nkind = \"usb-c\"\nside = \"left\"\n").unwrap_err().contains("hub"));
        assert!(parse_template("id = \"Not A Slug\"\nlabel = \"X\"\ncategory = \"board\"\nlook = \"board\"\n").unwrap_err().contains("not a slug"));
        let many: String = (0..=MAX_CONNECTORS).map(|i| format!("[[connectors]]\nid = \"c{i}\"\nlabel = \"C\"\nkind = \"swd\"\nside = \"left\"\n")).collect();
        assert!(with(&many).unwrap_err().contains("at most 64"));
    }
}
