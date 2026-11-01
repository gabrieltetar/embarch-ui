//! The bench's rule checks and the verdicts a cable being laid is drawn by
//! (proposal, "Rule checks"). **Pure functions over the description**: no
//! file, no Core, no hardware — every check reads what the bench says, never
//! what is plugged in.
//!
//! Two kinds of rule. **Refusals** — a cable of another kind, a connector past
//! `max_cables`, a role on two parts, one USB identity on two parts, two
//! providers on one bus — are what [`candidates`] refuses while a cable is
//! laid, and an `error` finding when a file already breaks one. **Warnings**
//! stay warnings: the picture must match the wiring "including when the
//! wiring is deliberately unusual" (decision 10).
//!
//! **A bus** is the usb-c, banana and terminal connectors joined by cables
//! and passthrough pairs: one VBUS, whichever connectors it runs through.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use embarch_core_client::SignalRoute;
use serde::Serialize;

use super::files::{Bench, Binding, Config, Part};
use super::{carries_power, joins, Connector, Function, Kind, PartTemplate, PowerClass, Role, HUB_KIND};

/// The templates a bench is checked against, by id.
pub type Templates = BTreeMap<String, PartTemplate>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// A refusal the description already breaks.
    Error,
    Warn,
    Info,
}

/// One finding. `cables` names each cable by its two ends, the shape a
/// config's cable already has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub level: Level,
    pub code: &'static str,
    pub text: String,
    pub parts: Vec<String>,
    pub cables: Vec<[String; 2]>,
}

fn finding(level: Level, code: &'static str, text: String, parts: Vec<String>, cables: Vec<[String; 2]>) -> Finding {
    Finding { level, code, text, parts, cables }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Ends at this machine; shared by every config.
    Bench,
    Config,
}

#[derive(Debug, Clone)]
struct Wire {
    a: String,
    b: String,
    scope: Scope,
}

impl Wire {
    fn touches(&self, end: &str) -> bool {
        self.a == end || self.b == end
    }

    fn other(&self, end: &str) -> &str {
        if self.a == end { &self.b } else { &self.a }
    }

    fn pair(&self) -> [String; 2] {
        [self.a.clone(), self.b.clone()]
    }
}

/// The cables in scope: the bench-level ones and the picked config's.
fn wires(bench: &Bench, config: Option<&Config>) -> Vec<Wire> {
    let mut out: Vec<Wire> = bench
        .cables
        .iter()
        .map(|c| Wire { a: c.from.clone(), b: c.to.clone(), scope: Scope::Bench })
        .collect();
    if let Some(config) = config {
        out.extend(config.cables.iter().map(|[a, b]| Wire { a: a.clone(), b: b.clone(), scope: Scope::Config }));
    }
    out
}

/// What a cable end names.
enum End<'a> {
    Hub(&'a Part),
    Conn(&'a Part, &'a Connector),
    /// The part is there but its template is not, so nothing can be said
    /// about its connectors (the part is kept and reported, decision 44).
    Unknowable(&'a Part),
    Bad(String),
}

fn part_id(end: &str) -> &str {
    end.split('.').next().unwrap_or(end)
}

struct Ctx<'a> {
    bench: &'a Bench,
    templates: &'a Templates,
}

impl<'a> Ctx<'a> {
    fn template(&self, part: &Part) -> Option<&'a PartTemplate> {
        self.templates.get(&part.template)
    }

    fn is_hub(&self, part: &Part) -> bool {
        self.template(part).is_some_and(|t| t.hub)
    }

    fn resolve(&self, end: &str) -> End<'a> {
        let (pid, cid) = match end.split_once('.') {
            Some((p, c)) => (p, Some(c)),
            None => (end, None),
        };
        let Some(part) = self.bench.part(pid) else {
            return End::Bad(format!("`{end}` names no part on this bench"));
        };
        let Some(template) = self.template(part) else {
            return End::Unknowable(part);
        };
        match (template.hub, cid) {
            (true, None) => End::Hub(part),
            (true, Some(_)) => End::Bad(format!(
                "`{end}`: {} is a hub, and a cable ends at `{pid}` itself, not at a connector on it",
                part.display()
            )),
            (false, None) => End::Bad(format!("`{end}` names {} but none of its connectors", part.display())),
            (false, Some(cid)) => match template.connector(cid) {
                Some(c) => End::Conn(part, c),
                None => End::Bad(format!("`{end}`: {} ({}) has no connector `{cid}`", part.display(), template.label)),
            },
        }
    }

    /// An end as a sentence names it: `drp · USB-C`, or a hub by its name.
    fn name(&self, end: &str) -> String {
        match self.resolve(end) {
            End::Hub(p) => p.display().to_string(),
            End::Conn(p, c) => format!("{} · {}", p.display(), c.label),
            End::Unknowable(_) | End::Bad(_) => end.to_string(),
        }
    }

    fn power_class(&self, end: &str) -> Option<PowerClass> {
        match self.resolve(end) {
            End::Conn(_, c) => c.power.map(|p| p.class()),
            _ => None,
        }
    }
}

// ---- buses ------------------------------------------------------------------

/// The power connectors on a bench, grouped into buses by a union-find.
struct Buses {
    index: BTreeMap<String, usize>,
    parent: Vec<usize>,
}

impl Buses {
    fn new(ctx: &Ctx, wires: &[Wire]) -> Buses {
        let mut buses = Buses { index: BTreeMap::new(), parent: Vec::new() };
        for part in &ctx.bench.parts {
            let Some(template) = ctx.template(part) else { continue };
            for c in template.connectors.iter().filter(|c| carries_power(c.kind)) {
                let key = format!("{}.{}", part.id, c.id);
                if !buses.index.contains_key(&key) {
                    buses.index.insert(key, buses.parent.len());
                    buses.parent.push(buses.parent.len());
                }
            }
            for [a, b] in &template.passthrough {
                buses.union(&format!("{}.{a}", part.id), &format!("{}.{b}", part.id));
            }
        }
        for w in wires {
            buses.union(&w.a, &w.b);
        }
        buses
    }

    fn find(&mut self, key: &str) -> Option<usize> {
        let mut i = *self.index.get(key)?;
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        Some(i)
    }

    fn union(&mut self, a: &str, b: &str) {
        if let (Some(x), Some(y)) = (self.find(a), self.find(b)) {
            self.parent[x] = y;
        }
    }

    /// Every bus, as its connectors' ends.
    fn groups(&mut self) -> Vec<Vec<String>> {
        let keys: Vec<String> = self.index.keys().cloned().collect();
        let mut by_root: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for k in keys {
            if let Some(root) = self.find(&k) {
                by_root.entry(root).or_default().push(k);
            }
        }
        by_root.into_values().collect()
    }

    fn group_of(&mut self, key: &str) -> Vec<String> {
        let Some(root) = self.find(key) else { return Vec::new() };
        let keys: Vec<String> = self.index.keys().cloned().collect();
        keys.into_iter().filter(|k| self.find(k) == Some(root)).collect()
    }
}

fn of_class(ctx: &Ctx, ends: &[String], class: PowerClass) -> Vec<String> {
    ends.iter().filter(|e| ctx.power_class(e) == Some(class)).cloned().collect()
}

fn names(ctx: &Ctx, ends: &[String]) -> String {
    let names: Vec<String> = ends.iter().map(|e| ctx.name(e)).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn parts_of(ends: &[String]) -> Vec<String> {
    let set: BTreeSet<&str> = ends.iter().map(|e| part_id(e)).collect();
    set.into_iter().map(str::to_string).collect()
}

// ---- USB identities ---------------------------------------------------------

/// One bound USB identity, resolved: what tells the device apart.
#[derive(Debug, Clone)]
struct Identity {
    part: String,
    end: String,
    function: Function,
    serial: Option<String>,
    interface: Option<u8>,
    vid: Option<u16>,
    pid: Option<u16>,
}

impl Identity {
    /// Whether two identities could be one physical device.
    fn same_device(&self, other: &Identity) -> bool {
        match (&self.serial, &other.serial) {
            (Some(a), Some(b)) => a == b,
            _ => {
                let compatible = |a: Option<u8>, b: Option<u8>| a.is_none() || b.is_none() || a == b;
                self.vid.is_some()
                    && self.vid == other.vid
                    && self.pid == other.pid
                    && compatible(self.interface, other.interface)
            }
        }
    }

    fn describe(&self) -> String {
        match (&self.serial, self.vid, self.pid) {
            (Some(s), _, _) => format!("USB serial {s}"),
            (None, Some(v), Some(p)) => format!("USB {v:04x}:{p:04x}"),
            _ => "an unnamed USB device".to_string(),
        }
    }
}

/// Every identity a part binds. A bare interface (`vcom = 1`) is on the same
/// device as that connector's probe; one with no probe beside it cannot be
/// resolved and is reported by [`check`].
fn identities(part: &Part) -> (Vec<Identity>, Vec<(String, Function, u8)>) {
    let mut out = Vec::new();
    let mut unresolved = Vec::new();
    for (cid, functions) in &part.usb {
        let end = format!("{}.{cid}", part.id);
        let probe_serial = match functions.get(&Function::Probe) {
            Some(Binding::Serial(s)) => Some(s.clone()),
            Some(Binding::Port(p)) => p.serial.clone(),
            _ => None,
        };
        for (&function, binding) in functions {
            let base = Identity { part: part.id.clone(), end: end.clone(), function, serial: None, interface: None, vid: None, pid: None };
            let identity = match binding {
                Binding::Serial(s) => Identity { serial: Some(s.clone()), ..base },
                Binding::Port(p) => Identity { serial: p.serial.clone(), interface: p.interface, vid: p.vid, pid: p.pid, ..base },
                Binding::Interface(i) => match &probe_serial {
                    Some(s) if function != Function::Probe => Identity { serial: Some(s.clone()), interface: Some(*i), ..base },
                    _ => {
                        unresolved.push((end.clone(), function, *i));
                        continue;
                    }
                },
            };
            out.push(identity);
        }
    }
    (out, unresolved)
}

// ---- the checks -------------------------------------------------------------

/// A role, as a sentence says it.
fn role_text(role: Role) -> &'static str {
    match role {
        Role::Dut => "DUT",
        Role::DevBench => "dev-bench",
    }
}

/// Whether a part's own probe reaches this machine: a `usb-host` carrying
/// `probe`, cabled to a hub.
fn probe_to_hub(ctx: &Ctx, wires: &[Wire], part: &Part) -> bool {
    let Some(template) = ctx.template(part) else { return false };
    template
        .connectors
        .iter()
        .filter(|c| c.kind == Kind::UsbHost && c.carries(Function::Probe))
        .any(|c| {
            let end = format!("{}.{}", part.id, c.id);
            wires
                .iter()
                .filter(|w| w.touches(&end))
                .any(|w| matches!(ctx.resolve(w.other(&end)), End::Hub(_)))
        })
}

/// A probe path: the part's own probe to this machine, or an SWD lead to a
/// part whose probe reaches it.
fn has_probe_path(ctx: &Ctx, wires: &[Wire], part: &Part) -> bool {
    if probe_to_hub(ctx, wires, part) {
        return true;
    }
    let Some(template) = ctx.template(part) else { return false };
    template.connectors.iter().filter(|c| c.kind == Kind::Swd).any(|c| {
        let end = format!("{}.{}", part.id, c.id);
        wires.iter().filter(|w| w.touches(&end)).any(|w| match ctx.resolve(w.other(&end)) {
            End::Conn(other, oc) => oc.kind == Kind::Swd && probe_to_hub(ctx, wires, other),
            _ => false,
        })
    })
}

/// Whether two parts are joined by part-to-part cables or a stack. This
/// machine is not a path: a USB host carries nothing between two devices.
fn joined(ctx: &Ctx, wires: &[Wire], from: &str, to: &str) -> bool {
    let is_hub = |id: &str| ctx.bench.part(id).is_none_or(|p| ctx.is_hub(p));
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for w in wires {
        let (a, b) = (part_id(&w.a), part_id(&w.b));
        if a != b && !is_hub(a) && !is_hub(b) {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }
    for p in &ctx.bench.parts {
        if let Some(base) = &p.mounted_on {
            adj.entry(p.id.as_str()).or_default().push(base.as_str());
            adj.entry(base.as_str()).or_default().push(p.id.as_str());
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::from([from]);
    let mut queue: VecDeque<&str> = VecDeque::from([from]);
    while let Some(at) = queue.pop_front() {
        if at == to {
            return true;
        }
        for &next in adj.get(at).map(Vec::as_slice).unwrap_or_default() {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    false
}

/// Every finding for one bench and config (`None`: the bench-level cables
/// alone). Errors first, then warnings.
pub fn check(bench: &Bench, templates: &Templates, config: Option<&Config>) -> Vec<Finding> {
    let ctx = Ctx { bench, templates };
    let wires = wires(bench, config);
    let mut out = Vec::new();

    // ---- parts ----
    let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
    for part in &bench.parts {
        if part.id.is_empty() || part.id.contains('.') || part.id.chars().any(char::is_whitespace) {
            out.push(finding(Level::Error, "bad-part-id",
                format!("Part id `{}` cannot name a cable end: it needs at least one character, no `.` and no space.", part.id),
                vec![part.id.clone()], vec![]));
        }
        if !seen_ids.insert(&part.id) {
            out.push(finding(Level::Error, "part-id-twice",
                format!("Two parts have the id `{}`; a cable end could mean either.", part.id), vec![part.id.clone()], vec![]));
        }
        if ctx.template(part).is_none() {
            out.push(finding(Level::Warn, "template-missing",
                format!("{} is drawn from the part template `{}`, which neither the built-ins nor embarch/parts/ has. It is kept as it is.", part.display(), part.template),
                vec![part.id.clone()], vec![]));
        }
    }

    // ---- stacking ----
    for part in &bench.parts {
        let Some(template) = ctx.template(part) else { continue };
        let has_plug = template.connectors.iter().any(|c| c.kind == Kind::StackPlug);
        match &part.mounted_on {
            None if has_plug => out.push(finding(Level::Warn, "shield-unmounted",
                format!("{} is mounted on nothing. Put it on a board with a free stack socket.", part.display()),
                vec![part.id.clone()], vec![])),
            None => {}
            Some(base_id) => {
                if !has_plug {
                    out.push(finding(Level::Error, "mount-no-plug",
                        format!("{} is mounted on {base_id}, but a {} has no stack plug to mount by.", part.display(), template.label),
                        vec![part.id.clone()], vec![]));
                }
                match bench.part(base_id) {
                    _ if base_id == &part.id => out.push(finding(Level::Error, "mount-unknown",
                        format!("{} is mounted on itself.", part.display()), vec![part.id.clone()], vec![])),
                    None => out.push(finding(Level::Error, "mount-unknown",
                        format!("{} is mounted on `{base_id}`, which is not a part on this bench.", part.display()),
                        vec![part.id.clone()], vec![])),
                    Some(base) => {
                        if let Some(bt) = ctx.template(base) {
                            if !bt.connectors.iter().any(|c| c.kind == Kind::StackSocket) {
                                out.push(finding(Level::Error, "mount-no-socket",
                                    format!("{} is mounted on {}, and a {} has no stack socket.", part.display(), base.display(), bt.label),
                                    vec![part.id.clone(), base.id.clone()], vec![]));
                            }
                        }
                    }
                }
            }
        }
    }
    for base in &bench.parts {
        let Some(bt) = ctx.template(base) else { continue };
        let room: u32 = bt.connectors.iter().filter(|c| c.kind == Kind::StackSocket).map(|c| c.max_cables).sum();
        let on: Vec<&Part> = bench.parts.iter().filter(|p| p.mounted_on.as_deref() == Some(base.id.as_str()) && p.id != base.id).collect();
        if room > 0 && on.len() as u32 > room {
            let mut parts = vec![base.id.clone()];
            parts.extend(on.iter().map(|p| p.id.clone()));
            out.push(finding(Level::Error, "mount-full",
                format!("{} parts are mounted on {}, which takes {room}.", on.len(), base.display()), parts, vec![]));
        }
    }

    // ---- roles ----
    for role in Role::ALL {
        let holders: Vec<&Part> = bench.parts.iter().filter(|p| p.role == Some(*role)).collect();
        if holders.len() > 1 {
            let who: Vec<&str> = holders.iter().map(|p| p.display()).collect();
            out.push(finding(Level::Error, "role-twice",
                format!("{} parts hold the {} role ({}); a role is one slot, held by one part.", holders.len(), role_text(*role), who.join(", ")),
                holders.iter().map(|p| p.id.clone()).collect(), vec![]));
        }
    }

    // ---- bindings ----
    let mut all_ids: Vec<Identity> = Vec::new();
    for part in &bench.parts {
        if let Some(template) = ctx.template(part) {
            for (cid, functions) in &part.usb {
                match template.connector(cid) {
                    Some(c) if c.kind == HUB_KIND => {
                        for function in functions.keys().filter(|f| !c.carries(**f)) {
                            out.push(finding(Level::Error, "binding-bad-function",
                                format!("{} · {} binds `{}`, which that connector does not carry.", part.display(), c.label, function.as_str()),
                                vec![part.id.clone()], vec![]));
                        }
                    }
                    _ => out.push(finding(Level::Error, "binding-bad-connector",
                        format!("{} binds USB functions on `{cid}`, which is not a usb-host connector of a {}.", part.display(), template.label),
                        vec![part.id.clone()], vec![])),
                }
            }
        }
        let (ids, unresolved) = identities(part);
        for (end, function, i) in unresolved {
            out.push(finding(Level::Warn, "binding-unresolved",
                format!("{} binds `{}` to interface {i}, but nothing on that connector names the device's serial.", ctx.name(&end), function.as_str()),
                vec![part.id.clone()], vec![]));
        }
        all_ids.extend(ids);
    }
    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
    for (i, a) in all_ids.iter().enumerate() {
        for b in &all_ids[i + 1..] {
            if a.part == b.part || !a.same_device(b) {
                continue;
            }
            let key = if a.part < b.part { (a.part.clone(), b.part.clone()) } else { (b.part.clone(), a.part.clone()) };
            if !reported.insert(key) {
                continue;
            }
            out.push(finding(Level::Error, "usb-identity-twice",
                format!("One USB device ({}) is bound on {} and on {}; a device is on one part.", a.describe(), ctx.name(&a.end), ctx.name(&b.end)),
                vec![a.part.clone(), b.part.clone()], vec![]));
        }
    }

    // ---- cables ----
    for w in &wires {
        let (ea, eb) = (ctx.resolve(&w.a), ctx.resolve(&w.b));
        let mut bad = false;
        for (end, resolved) in [(&w.a, &ea), (&w.b, &eb)] {
            if let End::Bad(why) = resolved {
                bad = true;
                out.push(finding(Level::Error, "unknown-end", format!("A cable end does not resolve: {why}."), parts_of(std::slice::from_ref(end)), vec![w.pair()]));
            }
        }
        if bad {
            continue;
        }
        let hub_end = matches!(ea, End::Hub(_)) || matches!(eb, End::Hub(_));
        match w.scope {
            Scope::Bench if !hub_end && !matches!(ea, End::Unknowable(_)) && !matches!(eb, End::Unknowable(_)) => {
                out.push(finding(Level::Error, "bench-cable-not-to-hub",
                    format!("The bench-level cable {} – {} does not end at this machine; a part-to-part cable belongs to a config.", ctx.name(&w.a), ctx.name(&w.b)),
                    parts_of(&[w.a.clone(), w.b.clone()]), vec![w.pair()]));
            }
            Scope::Config if hub_end => {
                out.push(finding(Level::Error, "hub-cable-in-config",
                    format!("{} – {} goes to this machine, so it is bench-level and shared by every config; a config holds part-to-part cables only.", ctx.name(&w.a), ctx.name(&w.b)),
                    parts_of(&[w.a.clone(), w.b.clone()]), vec![w.pair()]));
            }
            _ => {}
        }
        if part_id(&w.a) == part_id(&w.b) {
            out.push(finding(Level::Error, "cable-same-part",
                format!("{} – {} joins a part to itself; a cable joins two parts.", ctx.name(&w.a), ctx.name(&w.b)),
                parts_of(std::slice::from_ref(&w.a)), vec![w.pair()]));
            continue;
        }
        if let Some(why) = kind_refusal(&ctx, &ea, &eb, &w.a, &w.b) {
            out.push(finding(Level::Error, "kind-mismatch", why, parts_of(&[w.a.clone(), w.b.clone()]), vec![w.pair()]));
        }
    }
    let mut counted: BTreeSet<String> = BTreeSet::new();
    for w in &wires {
        for end in [&w.a, &w.b] {
            let End::Conn(part, c) = ctx.resolve(end) else { continue };
            if !counted.insert(end.clone()) {
                continue;
            }
            let on: Vec<&Wire> = wires.iter().filter(|x| x.touches(end)).collect();
            if on.len() as u32 > c.max_cables {
                out.push(finding(Level::Error, "connector-full",
                    format!("{} has {} cables and takes {}.", ctx.name(end), on.len(), c.max_cables),
                    vec![part.id.clone()], on.iter().map(|x| x.pair()).collect()));
            }
        }
    }

    // ---- configs ----
    let mut config_names: BTreeSet<&str> = BTreeSet::new();
    for c in &bench.configs {
        if !config_names.insert(&c.name) {
            out.push(finding(Level::Error, "config-name-twice",
                format!("Two configs are named `{}`; a study naming it could mean either.", c.name), vec![], vec![]));
        }
    }

    // ---- buses ----
    let mut buses = Buses::new(&ctx, &wires);
    for group in buses.groups() {
        let providers = of_class(&ctx, &group, PowerClass::Provider);
        let consumers = of_class(&ctx, &group, PowerClass::Consumer);
        let either = of_class(&ctx, &group, PowerClass::Either);
        let bus_cables: Vec<[String; 2]> = wires.iter().filter(|w| group.contains(&w.a) && group.contains(&w.b)).map(Wire::pair).collect();
        if providers.len() > 1 {
            out.push(finding(Level::Error, "two-providers",
                format!("Two sources on one bus: {}.", names(&ctx, &providers)), parts_of(&providers), bus_cables));
        } else if consumers.len() > 1 && providers.is_empty() && either.is_empty() {
            out.push(finding(Level::Warn, "no-source",
                format!("Nothing sources the bus joining {}.", names(&ctx, &consumers)), parts_of(&consumers), bus_cables));
        }
    }

    // ---- the X-NUCLEO-DRP1M1 sources only what CN3 gives it (dev-bench decision 49) ----
    for part in bench.parts.iter().filter(|p| p.template == "x-nucleo-drp1m1") {
        let Some(template) = ctx.template(part) else { continue };
        if template.connector("usb-c").is_none() || template.connector("cn3").is_none() {
            continue;
        }
        let own = |e: &String| part_id(e) == part.id;
        let port = buses.group_of(&format!("{}.usb-c", part.id));
        let partner_consumes = of_class(&ctx, &port, PowerClass::Consumer).iter().any(|e| !own(e));
        let partner_sources = of_class(&ctx, &port, PowerClass::Provider).iter().any(|e| !own(e));
        if partner_sources && template.connector("cn2").is_some() {
            // The mockup's note, kept as `info`: sinking with nothing on CN2
            // is a legitimate state (a contract read back, no load drawn).
            let cn2 = format!("{}.cn2", part.id);
            let loaded = of_class(&ctx, &buses.group_of(&cn2), PowerClass::Consumer).iter().any(|e| !own(e));
            if !loaded {
                out.push(finding(Level::Info, "drp-cn2-empty",
                    format!("The {} sinks on this wiring, and CN2 has no load on it.", template.label),
                    vec![part.id.clone()], vec![]));
            }
        }
        if !partner_consumes {
            continue;
        }
        let cn3 = format!("{}.cn3", part.id);
        let fed = of_class(&ctx, &buses.group_of(&cn3), PowerClass::Provider).iter().any(|e| !own(e));
        if fed {
            continue;
        }
        let text = if wires.iter().any(|w| w.touches(&cn3)) {
            format!("The {} sources only what CN3 gives it, and nothing on CN3's leads sources it.", template.label)
        } else {
            format!("The {} sources only what CN3 gives it, and CN3 has nothing on it.", template.label)
        };
        out.push(finding(Level::Warn, "drp-cn3-empty", text, vec![part.id.clone()], vec![]));
    }

    // ---- a probe path to this machine, for a role part or a bench board ----
    for part in &bench.parts {
        if ctx.template(part).is_none() || (part.role.is_none() && part.probe_serial().is_none()) {
            continue;
        }
        if !has_probe_path(&ctx, &wires, part) {
            let what = part.role.map(role_text).unwrap_or("bench board");
            out.push(finding(Level::Warn, "no-probe-path",
                format!("{} ({what}) has no probe path to this machine, so Core cannot flash or validate it.", part.display()),
                vec![part.id.clone()], vec![]));
        }
    }

    // ---- a signal's origin and its carrier ----
    for signal in &bench.signals {
        let origin = match signal.origin_board.as_deref() {
            Some(board) => bench.parts.iter().find(|p| p.probe_serial() == Some(board) || p.label.as_deref() == Some(board)),
            None => bench.parts.iter().find(|p| p.role.is_some_and(|r| r.as_str() == signal.origin_role)),
        };
        let (carrier, carried_by) = match &signal.route {
            SignalRoute::Direct { port_serial, interface } => {
                let at = match interface {
                    Some(i) => format!("the port {port_serial}, interface {i}"),
                    None => format!("the port {port_serial}"),
                };
                let carrier = all_ids
                    .iter()
                    .find(|id| {
                        id.function != Function::Probe
                            && id.serial.as_deref() == Some(port_serial.as_str())
                            && (interface.is_none() || id.interface.is_none() || id.interface == *interface)
                    })
                    .and_then(|id| bench.part(&id.part));
                (carrier, at)
            }
            SignalRoute::ViaDevBench { .. } => {
                (bench.parts.iter().find(|p| p.role == Some(Role::DevBench)), "the dev-bench pins".to_string())
            }
        };
        let leaves = signal.origin_board.clone().unwrap_or_else(|| signal.origin_role.clone());
        let text = match (origin, carrier) {
            (None, _) => format!("Signal `{}` leaves `{leaves}`, which is no part on this bench.", signal.name),
            (Some(o), None) => format!("Signal `{}` leaves {} on {carried_by}, which no part on this bench binds.", signal.name, o.display()),
            (Some(o), Some(c)) if !joined(&ctx, &wires, &o.id, &c.id) => format!(
                "Signal `{}` leaves {} and rides {carried_by} on {}, and no cable joins the two.",
                signal.name,
                o.display(),
                c.display()
            ),
            _ => continue,
        };
        let parts = [origin, carrier].into_iter().flatten().map(|p| p.id.clone()).collect::<BTreeSet<_>>().into_iter().collect();
        out.push(finding(Level::Warn, "signal-no-path", text, parts, vec![]));
    }

    out.sort_by_key(|f| f.level);
    out
}

/// Why two resolved ends cannot be joined by kind, or `None` when they can.
fn kind_refusal(ctx: &Ctx, ea: &End, eb: &End, a: &str, b: &str) -> Option<String> {
    let stack = |k: Kind| matches!(k, Kind::StackSocket | Kind::StackPlug);
    match (ea, eb) {
        (End::Hub(_), End::Hub(_)) => Some(format!("{} – {} joins this machine to itself.", ctx.name(a), ctx.name(b))),
        (End::Hub(_), End::Conn(_, c)) | (End::Conn(_, c), End::Hub(_)) if c.kind != HUB_KIND => {
            let end = if matches!(ea, End::Conn(..)) { a } else { b };
            Some(format!("{} is {}; only a usb-host joins this machine.", ctx.name(end), c.kind.as_str()))
        }
        (End::Conn(_, x), End::Conn(_, y)) => {
            if stack(x.kind) || stack(y.kind) {
                Some(format!(
                    "{} – {}: a stack connector is never cabled; a shield joins its board through mounted_on.",
                    ctx.name(a),
                    ctx.name(b)
                ))
            } else if x.kind == HUB_KIND || y.kind == HUB_KIND {
                Some(format!("{} – {}: a usb-host joins this machine, never another connector.", ctx.name(a), ctx.name(b)))
            } else if !joins(x.kind, y.kind) {
                Some(format!("{} is {} and {} is {}; those do not join.", ctx.name(a), x.kind.as_str(), ctx.name(b), y.kind.as_str()))
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---- candidates -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// Drawn pulsing: a cable may go here.
    Ok,
    /// Drawn red, saying why.
    Refused,
    /// Another kind: not drawn.
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub fit: Fit,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

impl Verdict {
    fn of(fit: Fit) -> Verdict {
        Verdict { fit, why: None }
    }

    fn refused(why: String) -> Verdict {
        Verdict { fit: Fit::Refused, why: Some(why) }
    }
}

/// Why an end that is already at its limit takes no more.
fn full_why(ctx: &Ctx, wires: &[Wire], end: &str) -> String {
    let on: Vec<&Wire> = wires.iter().filter(|w| w.touches(end)).collect();
    match on.as_slice() {
        [one] => format!("{} already goes to {}", ctx.name(end), ctx.name(one.other(end))),
        _ => format!("{} already has {} cables, the most it takes", ctx.name(end), on.len()),
    }
}

/// One verdict for every connector end on the bench, for a cable being laid
/// from `from`, so the browser decides nothing. `Err` when `from` is not an
/// end a cable can start at.
pub fn candidates(bench: &Bench, templates: &Templates, config: Option<&Config>, from: &str) -> Result<BTreeMap<String, Verdict>, String> {
    let ctx = Ctx { bench, templates };
    let wires = wires(bench, config);
    let from_end = ctx.resolve(from);
    match &from_end {
        End::Hub(_) | End::Conn(..) => {}
        End::Unknowable(p) => return Err(format!("`{from}`: {}'s template `{}` is missing, so its connectors are unknown", p.display(), p.template)),
        End::Bad(why) => return Err(why.clone()),
    }
    let count = |end: &str| wires.iter().filter(|w| w.touches(end)).count() as u32;
    let mut buses = Buses::new(&ctx, &wires);

    let mut ends: Vec<String> = Vec::new();
    for part in &bench.parts {
        let Some(template) = ctx.template(part) else { continue };
        if template.hub {
            ends.push(part.id.clone());
        } else {
            ends.extend(template.connectors.iter().map(|c| format!("{}.{}", part.id, c.id)));
        }
    }

    let stack = |k: Kind| matches!(k, Kind::StackSocket | Kind::StackPlug);
    let mut verdicts = BTreeMap::new();
    for to in ends {
        let to_end = ctx.resolve(&to);
        let same_part = part_id(&to) == part_id(from);
        let fits = !same_part
            && match (&from_end, &to_end) {
                (End::Hub(_), End::Conn(_, c)) | (End::Conn(_, c), End::Hub(_)) => c.kind == HUB_KIND,
                (End::Conn(_, x), End::Conn(_, y)) => {
                    !stack(x.kind) && !stack(y.kind) && x.kind != HUB_KIND && y.kind != HUB_KIND && joins(x.kind, y.kind)
                }
                _ => false,
            };
        if !fits {
            verdicts.insert(to, Verdict::of(Fit::Hidden));
            continue;
        }
        let verdict = 'v: {
            for (end, resolved) in [(from, &from_end), (to.as_str(), &to_end)] {
                if let End::Conn(_, c) = resolved {
                    if count(end) >= c.max_cables {
                        break 'v Verdict::refused(full_why(&ctx, &wires, end));
                    }
                }
            }
            if wires.iter().any(|w| (w.a == from && w.b == to) || (w.a == to && w.b == from)) {
                break 'v Verdict::refused(format!("{} and {} are already joined", ctx.name(from), ctx.name(&to)));
            }
            if let (End::Conn(_, x), End::Conn(_, y)) = (&from_end, &to_end) {
                if carries_power(x.kind) && carries_power(y.kind) {
                    let mut merged = buses.group_of(from);
                    for e in buses.group_of(&to) {
                        if !merged.contains(&e) {
                            merged.push(e);
                        }
                    }
                    merged.sort();
                    let providers = of_class(&ctx, &merged, PowerClass::Provider);
                    if providers.len() > 1 {
                        break 'v Verdict::refused(format!("Two sources on one bus: {}", names(&ctx, &providers)));
                    }
                }
            }
            Verdict::of(Fit::Ok)
        };
        verdicts.insert(to, verdict);
    }
    Ok(verdicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::builtins;
    use crate::bench::files::{Cable, PortBinding};
    use serde_json::json;

    fn templates() -> Templates {
        let mut t: Templates = builtins().into_iter().map(|t| (t.id.clone(), t)).collect();
        // A repo's PD DUT: two USB-C ports, an SWD header.
        let dut: PartTemplate = toml::from_str(
            r#"
id = "pd-dut"
label = "PD DUT"
category = "board"
look = "power-board"
[[connectors]]
id = "swd"
label = "SWD"
kind = "swd"
side = "left"
[[connectors]]
id = "usb-c-1"
label = "USB-C 1"
kind = "usb-c"
side = "left"
power = "source"
[[connectors]]
id = "usb-c-2"
label = "USB-C 2"
kind = "usb-c"
side = "left"
power = "drp"
"#,
        )
        .unwrap();
        t.insert(dut.id.clone(), dut);
        t
    }

    fn part(id: &str, template: &str) -> Part {
        serde_json::from_value(json!({ "id": id, "template": template, "at": [0, 0] })).unwrap()
    }

    fn cable(from: &str, to: &str) -> Cable {
        Cable { from: from.into(), to: to.into() }
    }

    fn pair(a: &str, b: &str) -> [String; 2] {
        [a.into(), b.into()]
    }

    /// The PD bench of the proposal: a partner Nucleo with the DRP shield, the
    /// analyzer in line, a load, a supply, a probe on the DUT.
    fn pd_bench() -> Bench {
        let mut partner = part("partner", "nucleo-g0b1re");
        partner.label = Some("pd-bench".into());
        partner.usb.insert("stlink".into(), BTreeMap::from([(Function::Probe, Binding::Serial("0671FF38".into())), (Function::Vcom, Binding::Interface(1))]));
        let mut drp = part("drp", "x-nucleo-drp1m1");
        drp.at = None;
        drp.mounted_on = Some("partner".into());
        let mut dut = part("dut", "pd-dut");
        dut.role = Some(Role::Dut);
        let mut probe = part("probe", "debug-probe");
        probe.usb.insert("usb".into(), BTreeMap::from([(Function::Probe, Binding::Serial("J1".into()))]));
        Bench {
            slug: "pd".into(),
            name: "PD".into(),
            legacy: false,
            parts: vec![part("host", "this-machine"), partner, drp, part("cy4500", "cy4500"), dut, probe, part("load", "scpi-load"), part("psu", "bench-supply")],
            cables: vec![cable("partner.stlink", "host"), cable("cy4500.usb", "host"), cable("probe.usb", "host"), cable("load.usb", "host")],
            configs: vec![
                Config { name: "A".into(), cables: vec![pair("dut.usb-c-1", "cy4500.in"), pair("cy4500.out", "drp.usb-c"), pair("drp.cn2", "load.in"), pair("probe.swd", "dut.swd")] },
                Config { name: "B".into(), cables: vec![pair("dut.usb-c-2", "drp.usb-c"), pair("psu.out", "drp.cn3"), pair("probe.swd", "dut.swd")] },
            ],
            signals: vec![],
        }
    }

    fn codes(findings: &[Finding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.code).collect()
    }

    #[test]
    fn a_wired_pd_bench_checks_clean() {
        let bench = pd_bench();
        for config in &bench.configs {
            let f = check(&bench, &templates(), Some(config));
            assert!(f.is_empty(), "config {}: {f:#?}", config.name);
        }
    }

    #[test]
    fn the_drp_with_a_consumer_partner_and_nothing_on_cn3_warns() {
        let mut bench = pd_bench();
        // The DUT's port 2 is a DRP; make the partner a sink through the analyzer instead.
        bench.parts.push(part("snk", "x-nucleo-snk1m1"));
        bench.configs[1].cables = vec![pair("snk.usb-c", "cy4500.in"), pair("cy4500.out", "drp.usb-c"), pair("probe.swd", "dut.swd")];
        let f = check(&bench, &templates(), bench.config("B"));
        let drp = f.iter().find(|f| f.code == "drp-cn3-empty").expect("warned");
        assert_eq!(drp.level, Level::Warn);
        assert_eq!(drp.text, "The X-NUCLEO-DRP1M1 sources only what CN3 gives it, and CN3 has nothing on it.");
        assert_eq!(drp.parts, ["drp"]);
        // A supply on CN3 answers it.
        bench.configs[1].cables.push(pair("psu.out", "drp.cn3"));
        assert!(!codes(&check(&bench, &templates(), bench.config("B"))).contains(&"drp-cn3-empty"));
    }

    #[test]
    fn the_drp_sinking_with_no_load_on_cn2_is_only_noted() {
        let mut bench = pd_bench();
        bench.configs[0].cables.retain(|c| c[0] != "drp.cn2");
        let f = check(&bench, &templates(), bench.config("A"));
        assert_eq!(codes(&f), ["drp-cn2-empty"]);
        assert_eq!(f[0].level, Level::Info);
    }

    #[test]
    fn two_sources_on_one_bus_are_an_error_through_a_passthrough() {
        let mut bench = pd_bench();
        bench.parts.push(part("brk", "usb-c-breakout"));
        // The DUT sources port 1 through the analyzer and the breakout; the supply sources the banana side.
        bench.configs[0].cables = vec![pair("dut.usb-c-1", "cy4500.in"), pair("cy4500.out", "brk.usb-c"), pair("brk.banana", "psu.out"), pair("probe.swd", "dut.swd")];
        let f = check(&bench, &templates(), bench.config("A"));
        let clash = f.iter().find(|f| f.code == "two-providers").expect("refused");
        assert_eq!(clash.level, Level::Error);
        assert_eq!(clash.text, "Two sources on one bus: dut · USB-C 1 and psu · +/- out.");
        assert_eq!(clash.parts, ["dut", "psu"]);
        assert_eq!(clash.cables.len(), 3);
    }

    #[test]
    fn two_consumers_and_no_source_warn_but_a_drp_could_source() {
        let mut bench = pd_bench();
        bench.parts.push(part("snk", "x-nucleo-snk1m1"));
        bench.configs[0].cables = vec![pair("snk.out", "load.in"), pair("probe.swd", "dut.swd")];
        // `out` is a provider: nothing to warn.
        assert!(!codes(&check(&bench, &templates(), bench.config("A"))).contains(&"no-source"));
        bench.configs[0].cables = vec![pair("snk.usb-c", "cy4500.in"), pair("cy4500.out", "drp.usb-c"), pair("probe.swd", "dut.swd")];
        // Sink + DRP: the DRP may source.
        assert!(!codes(&check(&bench, &templates(), bench.config("A"))).contains(&"no-source"));
        let mut two = part("snk2", "x-nucleo-snk1m1");
        two.mounted_on = Some("partner".into());
        bench.parts.push(two);
        bench.configs[0].cables = vec![pair("snk.usb-c", "cy4500.in"), pair("cy4500.out", "snk2.usb-c"), pair("probe.swd", "dut.swd")];
        let f = check(&bench, &templates(), bench.config("A"));
        assert!(codes(&f).contains(&"no-source"), "{f:#?}");
    }

    #[test]
    fn refusals_a_file_already_breaks_are_errors() {
        let mut bench = pd_bench();
        bench.parts[7].role = Some(Role::Dut); // the supply now also holds the DUT role
        bench.parts[6].usb.insert("usb".into(), BTreeMap::from([(Function::Scpi, Binding::Serial("0671FF38".into()))]));
        bench.cables.push(cable("dut.usb-c-1", "cy4500.in"));
        bench.configs[0].cables.push(pair("cy4500.usb", "host"));
        bench.configs[0].cables.push(pair("dut.swd", "cy4500.out"));
        bench.configs[0].cables.push(pair("load.in", "psu.out"));
        bench.configs[0].cables.push(pair("nope.x", "psu.out"));
        let f = check(&bench, &templates(), bench.config("A"));
        let c = codes(&f);
        for code in ["role-twice", "usb-identity-twice", "bench-cable-not-to-hub", "hub-cable-in-config", "kind-mismatch", "connector-full", "unknown-end"] {
            assert!(c.contains(&code), "{code} missing from {f:#?}");
        }
        assert!(f.iter().take_while(|f| f.level == Level::Error).count() >= 7, "errors sort first");
        let twice = f.iter().find(|f| f.code == "usb-identity-twice").unwrap();
        assert!(twice.text.contains("USB serial 0671FF38"), "{}", twice.text);
    }

    #[test]
    fn a_shield_on_nothing_and_a_missing_template_warn_and_keep_the_part() {
        let mut bench = pd_bench();
        bench.parts[2].mounted_on = None;
        bench.parts.push(part("ghost", "gone-board"));
        let f = check(&bench, &templates(), bench.config("A"));
        assert!(codes(&f).contains(&"shield-unmounted"));
        let missing = f.iter().find(|f| f.code == "template-missing").unwrap();
        assert_eq!((missing.level, missing.parts.as_slice()), (Level::Warn, &["ghost".to_string()][..]));
        bench.parts[2].mounted_on = Some("cy4500".into());
        assert!(codes(&check(&bench, &templates(), None)).contains(&"mount-no-socket"));
    }

    #[test]
    fn a_role_part_with_no_probe_path_warns_and_an_swd_lead_is_one() {
        let bench = pd_bench();
        assert!(!codes(&check(&bench, &templates(), bench.config("A"))).contains(&"no-probe-path"));
        let mut cut = bench.clone();
        cut.configs[0].cables.retain(|c| c[0] != "probe.swd");
        let f = check(&cut, &templates(), cut.config("A"));
        let w = f.iter().find(|f| f.code == "no-probe-path").unwrap();
        assert_eq!(w.text, "dut (DUT) has no probe path to this machine, so Core cannot flash or validate it.");
        // A bench board with its probe bound and no cable home warns too.
        let mut home = bench.clone();
        home.cables.retain(|c| c.from != "partner.stlink");
        let f = check(&home, &templates(), home.config("A"));
        assert!(f.iter().any(|f| f.code == "no-probe-path" && f.text.starts_with("pd-bench (bench board)")), "{f:#?}");
    }

    #[test]
    fn a_signal_needs_a_cable_path_from_its_origin_to_its_carrier() {
        let mut bench = pd_bench();
        let mut bridge = part("ftdi", "usb-uart-bridge");
        bridge.usb.insert("usb".into(), BTreeMap::from([(Function::Vcom, Binding::Port(PortBinding { serial: Some("FT9".into()), ..Default::default() }))]));
        bench.parts.push(bridge);
        bench.cables.push(cable("ftdi.usb", "host"));
        let console: embarch_core_client::SignalLink = serde_json::from_value(json!({
            "name": "console", "origin_role": "dut", "direction": "bidirectional",
            "route": { "kind": "direct", "port_serial": "FT9" }
        }))
        .unwrap();
        bench.signals.push(console);
        let f = check(&bench, &templates(), bench.config("A"));
        let w = f.iter().find(|f| f.code == "signal-no-path").expect("warned");
        assert!(w.text.contains("no cable joins the two"), "{}", w.text);
        // The DUT has no pin header, so the path only exists through a repo template; a
        // bench board's own VCOM carrying its own shell needs no cable at all.
        bench.signals[0] = serde_json::from_value(json!({
            "name": "shell", "origin_role": "", "origin_board": "pd-bench", "direction": "bidirectional",
            "route": { "kind": "direct", "port_serial": "0671FF38", "interface": 1 }
        }))
        .unwrap();
        assert!(!codes(&check(&bench, &templates(), bench.config("A"))).contains(&"signal-no-path"));
        // A carrier no part binds says so.
        bench.signals[0] = serde_json::from_value(json!({
            "name": "x", "origin_role": "dut", "direction": "bidirectional",
            "route": { "kind": "direct", "port_serial": "NOPE" }
        }))
        .unwrap();
        let f = check(&bench, &templates(), bench.config("A"));
        assert!(f.iter().any(|f| f.code == "signal-no-path" && f.text.contains("which no part on this bench binds")), "{f:#?}");
    }

    #[test]
    fn a_cable_from_a_usb_c_port_gets_one_verdict_per_end() {
        let bench = pd_bench();
        let v = candidates(&bench, &templates(), bench.config("A"), "dut.usb-c-2").unwrap();
        assert_eq!(v["cy4500.in"].fit, Fit::Refused, "taken by port 1");
        assert_eq!(v["drp.usb-c"], Verdict::refused("drp · USB-C already goes to cy4500 · USB-C".into()));
        assert_eq!(v["drp.cn3"].fit, Fit::Hidden, "another kind");
        assert_eq!(v["host"].fit, Fit::Hidden);
        assert_eq!(v["dut.usb-c-1"].fit, Fit::Hidden, "same part");
        assert_eq!(v["partner.stack"].fit, Fit::Hidden);
        // Every end on the bench is answered.
        let ends: usize = bench.parts.iter().map(|p| {
            let t = &templates()[&p.template];
            if t.hub { 1 } else { t.connectors.len() }
        }).sum();
        assert_eq!(v.len(), ends);
        assert!(serde_json::to_value(&v).unwrap()["drp.cn3"] == json!({ "fit": "hidden" }));
    }

    #[test]
    fn a_cable_that_would_put_two_sources_on_a_bus_is_refused() {
        let mut bench = pd_bench();
        bench.parts.push(part("brk", "usb-c-breakout"));
        bench.configs[0].cables = vec![pair("dut.usb-c-1", "cy4500.in"), pair("cy4500.out", "brk.usb-c"), pair("probe.swd", "dut.swd")];
        let v = candidates(&bench, &templates(), bench.config("A"), "psu.out").unwrap();
        assert_eq!(v["brk.banana"], Verdict::refused("Two sources on one bus: dut · USB-C 1 and psu · +/- out".into()));
        assert_eq!(v["load.in"].fit, Fit::Ok);
        assert_eq!(v["drp.cn3"].fit, Fit::Ok, "banana joins terminal");
        assert_eq!(v["drp.cn2"].fit, Fit::Refused, "out to supply is two sources");
    }

    #[test]
    fn a_usb_host_joins_this_machine_and_nothing_else() {
        let mut bench = pd_bench();
        bench.cables.retain(|c| c.from != "cy4500.usb");
        let v = candidates(&bench, &templates(), None, "cy4500.usb").unwrap();
        assert_eq!(v["host"].fit, Fit::Ok);
        assert!(v.iter().filter(|(k, _)| *k != "host").all(|(_, v)| v.fit == Fit::Hidden));
        let v = candidates(&bench, &templates(), None, "host").unwrap();
        assert_eq!(v["cy4500.usb"].fit, Fit::Ok);
        assert_eq!(v["partner.stlink"], Verdict::refused("pd-bench · ST-LINK USB already goes to host".into()));
        assert_eq!(v["dut.usb-c-1"].fit, Fit::Hidden);
        assert!(candidates(&bench, &templates(), None, "nope.x").is_err());
        assert!(candidates(&bench, &templates(), None, "host.x").is_err());
    }
}
