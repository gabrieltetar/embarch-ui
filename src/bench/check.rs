//! The bench's rule checks and the verdicts a cable being laid is drawn by
//! (proposal, "Rule checks"). **Pure functions over the description**: no
//! file, no Core, no hardware — every check reads what the bench says, never
//! what is plugged in.
//!
//! Two kinds of rule. **Refusals** — a cable of another kind, a connector past
//! `max_cables`, a role on two parts, one USB identity on two parts, two
//! providers on one bus — are what [`candidates`] refuses while a cable is
//! laid, and an `error` finding when a file already breaks one; from a stack
//! plug, [`candidates`] answers where its part may be mounted instead (full
//! headers, a ring). **Warnings**
//! stay warnings: the picture must match the wiring "including when the
//! wiring is deliberately unusual" (decision 10).
//!
//! **A bus** is the usb-c, banana and terminal connectors joined by cables
//! and passthrough pairs: one VBUS, whichever connectors it runs through.
//!
//! Everything a check looks up repeatedly — a part by id, the cables on an
//! end, the bus an end is on — is indexed once per request, so a bench at
//! the size cap (`files::MAX_PARTS`) costs a few lookups per rule, not a scan.

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
    fn other(&self, end: &str) -> &str {
        if self.a == end { &self.b } else { &self.a }
    }

    fn pair(&self) -> [String; 2] {
        [self.a.clone(), self.b.clone()]
    }
}

/// The cables in scope — the bench-level ones and the picked config's — and
/// which of them touch each end.
struct Wiring {
    wires: Vec<Wire>,
    on_end: BTreeMap<String, Vec<usize>>,
}

impl Wiring {
    fn new(bench: &Bench, config: Option<&Config>) -> Wiring {
        let mut wires: Vec<Wire> = bench
            .cables
            .iter()
            .map(|c| Wire { a: c.from.clone(), b: c.to.clone(), scope: Scope::Bench })
            .collect();
        if let Some(config) = config {
            wires.extend(config.cables.iter().map(|[a, b]| Wire { a: a.clone(), b: b.clone(), scope: Scope::Config }));
        }
        let mut on_end: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, w) in wires.iter().enumerate() {
            on_end.entry(w.a.clone()).or_default().push(i);
            if w.b != w.a {
                on_end.entry(w.b.clone()).or_default().push(i);
            }
        }
        Wiring { wires, on_end }
    }

    /// The cables touching an end.
    fn at<'w>(&'w self, end: &str) -> impl Iterator<Item = &'w Wire> + 'w {
        self.on_end.get(end).into_iter().flatten().map(move |&i| &self.wires[i])
    }

    fn count(&self, end: &str) -> usize {
        self.on_end.get(end).map_or(0, Vec::len)
    }

    fn joins_pair(&self, a: &str, b: &str) -> bool {
        self.at(a).any(|w| w.other(a) == b)
    }
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
    templates: &'a Templates,
    /// The first part with each id; a second is reported as `part-id-twice`.
    by_id: BTreeMap<&'a str, &'a Part>,
}

impl<'a> Ctx<'a> {
    fn new(bench: &'a Bench, templates: &'a Templates) -> Ctx<'a> {
        let mut by_id = BTreeMap::new();
        for p in &bench.parts {
            by_id.entry(p.id.as_str()).or_insert(p);
        }
        Ctx { templates, by_id }
    }

    fn part(&self, id: &str) -> Option<&'a Part> {
        self.by_id.get(id).copied()
    }

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
        let Some(part) = self.part(pid) else {
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

/// The power connectors on a bench grouped into buses: a union-find run
/// once, then frozen into a lookup from end to bus.
struct Buses {
    bus_of: BTreeMap<String, usize>,
    members: Vec<Vec<String>>,
}

impl Buses {
    fn new(ctx: &Ctx, bench: &Bench, wiring: &Wiring) -> Buses {
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        let mut parent: Vec<usize> = Vec::new();
        for part in &bench.parts {
            let Some(template) = ctx.template(part) else { continue };
            for c in template.connectors.iter().filter(|c| carries_power(c.kind)) {
                let key = format!("{}.{}", part.id, c.id);
                if let std::collections::btree_map::Entry::Vacant(slot) = index.entry(key) {
                    slot.insert(parent.len());
                    parent.push(parent.len());
                }
            }
        }
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut union = |a: &str, b: &str| {
            if let (Some(&x), Some(&y)) = (index.get(a), index.get(b)) {
                let (rx, ry) = (find(&mut parent, x), find(&mut parent, y));
                parent[rx] = ry;
            }
        };
        for part in &bench.parts {
            let Some(template) = ctx.template(part) else { continue };
            for [a, b] in &template.passthrough {
                union(&format!("{}.{a}", part.id), &format!("{}.{b}", part.id));
            }
        }
        for w in &wiring.wires {
            union(&w.a, &w.b);
        }
        let mut bus_of = BTreeMap::new();
        let mut by_root: BTreeMap<usize, usize> = BTreeMap::new();
        let mut members: Vec<Vec<String>> = Vec::new();
        for (key, &i) in &index {
            let root = find(&mut parent, i);
            let bus = *by_root.entry(root).or_insert_with(|| {
                members.push(Vec::new());
                members.len() - 1
            });
            members[bus].push(key.clone());
            bus_of.insert(key.clone(), bus);
        }
        Buses { bus_of, members }
    }

    /// The ends on the same bus as `key`, sorted; empty for an end that
    /// carries no power.
    fn group_of(&self, key: &str) -> &[String] {
        self.bus_of.get(key).map(|&b| self.members[b].as_slice()).unwrap_or_default()
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

/// The first two identities from different parts seen under one key, so
/// "is there one from another part?" is answered without a pair scan.
#[derive(Default)]
struct TwoParts<'i> {
    first: Option<&'i Identity>,
    second: Option<&'i Identity>,
}

impl<'i> TwoParts<'i> {
    fn add(&mut self, id: &'i Identity) {
        match self.first {
            None => self.first = Some(id),
            Some(f) if f.part != id.part && self.second.is_none() => self.second = Some(id),
            _ => {}
        }
    }

    fn other_than(&self, part: &str) -> Option<&'i Identity> {
        match self.first {
            Some(f) if f.part != part => Some(f),
            _ => self.second.filter(|s| s.part != part),
        }
    }
}

/// One USB device bound on two parts, or two that cannot be told apart, as
/// findings — **without comparing every pair**: one finding per serial bound
/// on more than one part, and per serial-less identity, the first identity on
/// another part a port matching it would also match (same VID:PID, an
/// interface that does not differ).
fn identity_clashes(ctx: &Ctx, ids: &[Identity]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut by_serial: BTreeMap<&str, Vec<&Identity>> = BTreeMap::new();
    for id in ids {
        if let Some(serial) = id.serial.as_deref() {
            let on = by_serial.entry(serial).or_default();
            if !on.iter().any(|o| o.part == id.part) {
                on.push(id);
            }
        }
    }
    for (serial, on) in by_serial.iter().filter(|(_, on)| on.len() > 1) {
        let ends: Vec<String> = on.iter().map(|i| i.end.clone()).collect();
        out.push(finding(Level::Error, "usb-identity-twice",
            format!("One USB device (USB serial {serial}) is bound on {}; a device is on one part.", names(ctx, &ends)),
            on.iter().map(|i| i.part.clone()).collect(), vec![]));
    }

    #[derive(Default)]
    struct Bucket<'i> {
        any: TwoParts<'i>,
        by_interface: BTreeMap<Option<u8>, TwoParts<'i>>,
    }
    // Keyed on the VID and the PID *as declared*, so two VID-only bindings
    // (a bridge whose product id nobody wrote down) are compared too.
    let mut buckets: BTreeMap<(u16, Option<u16>), Bucket> = BTreeMap::new();
    for id in ids {
        if let Some(vid) = id.vid {
            let b = buckets.entry((vid, id.pid)).or_default();
            b.any.add(id);
            b.by_interface.entry(id.interface).or_default().add(id);
        }
    }
    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
    for id in ids.iter().filter(|i| i.serial.is_none()) {
        let Some(vid) = id.vid else { continue };
        let b = &buckets[&(vid, id.pid)];
        let other = match id.interface {
            None => b.any.other_than(&id.part),
            Some(i) => b
                .by_interface
                .get(&Some(i))
                .and_then(|t| t.other_than(&id.part))
                .or_else(|| b.by_interface.get(&None).and_then(|t| t.other_than(&id.part))),
        };
        let Some(other) = other else { continue };
        let key = if id.part < other.part { (id.part.clone(), other.part.clone()) } else { (other.part.clone(), id.part.clone()) };
        if reported.insert(key) {
            out.push(finding(Level::Error, "usb-identity-twice", identity_clash_text(ctx, id, other), vec![id.part.clone(), other.part.clone()], vec![]));
        }
    }
    out
}

/// The sentence for a serial-less identity `a` that a port matching `b`
/// would also match.
fn identity_clash_text(ctx: &Ctx, a: &Identity, b: &Identity) -> String {
    let usb = match a.pid {
        Some(pid) => format!("USB {:04x}:{pid:04x}", a.vid.unwrap_or_default()),
        None => format!("USB vendor {:04x} (no product id)", a.vid.unwrap_or_default()),
    };
    match &b.serial {
        Some(serial) => format!(
            "{} binds {usb} with serial {serial}, and {} the same {usb} with no serial and no interface that \
             differs, so a port matching the second also matches the first; give it its serial or an interface.",
            ctx.name(&b.end),
            ctx.name(&a.end)
        ),
        None => format!(
            "{} and {} both bind {usb} with no serial and no interface that differs, so the two cannot be told \
             apart; give one a serial or an interface.",
            ctx.name(&a.end),
            ctx.name(&b.end)
        ),
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
fn probe_to_hub(ctx: &Ctx, wiring: &Wiring, part: &Part) -> bool {
    let Some(template) = ctx.template(part) else { return false };
    template
        .connectors
        .iter()
        .filter(|c| c.kind == Kind::UsbHost && c.carries(Function::Probe))
        .any(|c| {
            let end = format!("{}.{}", part.id, c.id);
            wiring.at(&end).any(|w| matches!(ctx.resolve(w.other(&end)), End::Hub(_)))
        })
}

/// A probe path: the part's own probe to this machine, or an SWD lead to a
/// part whose probe reaches it.
fn has_probe_path(ctx: &Ctx, wiring: &Wiring, part: &Part) -> bool {
    if probe_to_hub(ctx, wiring, part) {
        return true;
    }
    let Some(template) = ctx.template(part) else { return false };
    template.connectors.iter().filter(|c| c.kind == Kind::Swd).any(|c| {
        let end = format!("{}.{}", part.id, c.id);
        wiring.at(&end).any(|w| match ctx.resolve(w.other(&end)) {
            End::Conn(other, oc) => oc.kind == Kind::Swd && probe_to_hub(ctx, wiring, other),
            _ => false,
        })
    })
}

/// Which parts are joined to which by part-to-part cables or a stack. This
/// machine is not a path: a USB host carries nothing between two devices.
fn adjacency<'b>(ctx: &Ctx, bench: &'b Bench, wiring: &'b Wiring) -> BTreeMap<&'b str, Vec<&'b str>> {
    let is_hub = |id: &str| ctx.part(id).is_none_or(|p| ctx.is_hub(p));
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for w in &wiring.wires {
        let (a, b) = (part_id(&w.a), part_id(&w.b));
        if a != b && !is_hub(a) && !is_hub(b) {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }
    for p in &bench.parts {
        if let Some(base) = &p.mounted_on {
            adj.entry(p.id.as_str()).or_default().push(base.as_str());
            adj.entry(base.as_str()).or_default().push(p.id.as_str());
        }
    }
    adj
}

fn joined(adj: &BTreeMap<&str, Vec<&str>>, from: &str, to: &str) -> bool {
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

/// Every chain of `mounted_on` that comes back to where it started, each
/// once, as its parts in chain order from the smallest id.
fn mount_cycles(ctx: &Ctx, bench: &Bench) -> Vec<Vec<String>> {
    fn canonical(mut chain: Vec<&str>) -> Vec<String> {
        let min = chain.iter().enumerate().min_by_key(|(_, id)| **id).map_or(0, |(i, _)| i);
        chain.rotate_left(min);
        chain.into_iter().map(str::to_string).collect()
    }
    let mut cycles: BTreeSet<Vec<String>> = BTreeSet::new();
    // The parts a mount can name — the first under each id — as nodes, each
    // with at most one edge, to what it is mounted on: a functional graph,
    // whose rings one walk with white/grey/black marks finds (fix round 2;
    // a walk per part, checking its chain at each step, was cubic).
    let nodes: Vec<&Part> = bench.parts.iter().filter(|p| ctx.part(&p.id).is_some_and(|q| std::ptr::eq(q, *p))).collect();
    let index: BTreeMap<&str, usize> = nodes.iter().enumerate().map(|(i, p)| (p.id.as_str(), i)).collect();
    let next = |i: usize| nodes[i].mounted_on.as_deref().and_then(|b| index.get(b).copied());
    const WHITE: u8 = 0;
    const GREY: u8 = 1;
    const BLACK: u8 = 2;
    let mut mark = vec![WHITE; nodes.len()];
    for first in 0..nodes.len() {
        if mark[first] != WHITE {
            continue;
        }
        let mut path: Vec<usize> = Vec::new();
        let mut at = Some(first);
        while let Some(i) = at {
            match mark[i] {
                WHITE => {
                    mark[i] = GREY;
                    path.push(i);
                    at = next(i);
                }
                GREY => {
                    let from = path.iter().position(|&j| j == i).unwrap_or(0);
                    cycles.insert(canonical(path[from..].iter().map(|&j| nodes[j].id.as_str()).collect()));
                    break;
                }
                _ => break,
            }
        }
        for j in path {
            mark[j] = BLACK;
        }
    }
    // A second part under an id is no node, but its own mount still starts a
    // chain, which is a ring when it comes back to its id.
    for start in bench.parts.iter().filter(|p| !ctx.part(&p.id).is_some_and(|q| std::ptr::eq(q, *p))) {
        let mut chain: Vec<&str> = vec![start.id.as_str()];
        let mut seen: BTreeSet<&str> = BTreeSet::from([start.id.as_str()]);
        let mut at = start;
        while let Some(next) = at.mounted_on.as_deref().and_then(|b| ctx.part(b)) {
            if next.id == start.id {
                cycles.insert(canonical(chain));
                break;
            }
            if !seen.insert(next.id.as_str()) {
                break;
            }
            chain.push(&next.id);
            at = next;
        }
    }
    cycles.into_iter().collect()
}

/// Every finding for one bench and config (`None`: the bench-level cables
/// alone). Errors first, then warnings, then notes.
pub fn check(bench: &Bench, templates: &Templates, config: Option<&Config>) -> Vec<Finding> {
    let ctx = Ctx::new(bench, templates);
    let wiring = Wiring::new(bench, config);
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
    for cycle in mount_cycles(&ctx, bench) {
        let text = match cycle.as_slice() {
            [one] => format!("{} is mounted on itself.", ctx.part(one).map_or(one.as_str(), |p| p.display())),
            _ => format!(
                "{} are mounted on each other in a ring; a stack has a board at the bottom.",
                cycle.iter().map(|id| ctx.part(id).map_or(id.as_str(), |p| p.display())).collect::<Vec<_>>().join(" → ")
            ),
        };
        out.push(finding(Level::Error, "mount-cycle", text, cycle, vec![]));
    }
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
                if base_id == &part.id {
                    continue;
                }
                match ctx.part(base_id) {
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
    for (base_id, on) in &mounts_by_board(bench) {
        let Some(base) = ctx.part(base_id) else { continue };
        let Some(bt) = ctx.template(base) else { continue };
        let room = stack_room(bt);
        if room > 0 && on.len() > room {
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
    out.extend(identity_clashes(&ctx, &all_ids));

    // ---- cables ----
    for w in &wiring.wires {
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
    for end in wiring.on_end.keys() {
        let End::Conn(part, c) = ctx.resolve(end) else { continue };
        let n = wiring.count(end);
        if n > c.max_cables as usize {
            out.push(finding(Level::Error, "connector-full",
                format!("{} has {n} cables and takes {}.", ctx.name(end), c.max_cables),
                vec![part.id.clone()], wiring.at(end).map(Wire::pair).collect()));
        }
    }

    // ---- configs ----
    let mut config_names: BTreeSet<&str> = BTreeSet::new();
    for c in &bench.configs {
        // A name a host study could not write (a hand edit): drawn and
        // checked, and refused on save in the same sentence, said here so the
        // refusal is on screen before Save (decision 72). Renaming fixes it.
        if let Err(why) = super::files::check_config_name(&c.name) {
            out.push(finding(Level::Error, "config-name-refused", format!("{why}. Rename it to save the bench."), vec![], vec![]));
        }
        if !config_names.insert(&c.name) {
            out.push(finding(Level::Error, "config-name-twice",
                format!("Two configs are named `{}`; a study naming it could mean either.", c.name), vec![], vec![]));
        }
    }

    // ---- buses ----
    let buses = Buses::new(&ctx, bench, &wiring);
    for group in &buses.members {
        let providers = of_class(&ctx, group, PowerClass::Provider);
        let consumers = of_class(&ctx, group, PowerClass::Consumer);
        let either = of_class(&ctx, group, PowerClass::Either);
        let on_bus: BTreeSet<&str> = group.iter().map(String::as_str).collect();
        let bus_cables: Vec<[String; 2]> = group
            .iter()
            .flat_map(|e| wiring.at(e))
            .filter(|w| on_bus.contains(w.a.as_str()) && on_bus.contains(w.b.as_str()))
            .map(Wire::pair)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
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
        let partner_consumes = of_class(&ctx, port, PowerClass::Consumer).iter().any(|e| !own(e));
        let partner_sources = of_class(&ctx, port, PowerClass::Provider).iter().any(|e| !own(e));
        if partner_sources && template.connector("cn2").is_some() {
            // The mockup's note, kept as `info`: sinking with nothing on CN2
            // is a legitimate state (a contract read back, no load drawn).
            let cn2 = format!("{}.cn2", part.id);
            let loaded = of_class(&ctx, buses.group_of(&cn2), PowerClass::Consumer).iter().any(|e| !own(e));
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
        let fed = of_class(&ctx, buses.group_of(&cn3), PowerClass::Provider).iter().any(|e| !own(e));
        if fed {
            continue;
        }
        let text = if wiring.count(&cn3) > 0 {
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
        if !has_probe_path(&ctx, &wiring, part) {
            let what = part.role.map(role_text).unwrap_or("bench board");
            out.push(finding(Level::Warn, "no-probe-path",
                format!("{} ({what}) has no probe path to this machine, so Core cannot flash or validate it.", part.display()),
                vec![part.id.clone()], vec![]));
        }
    }

    // ---- a signal's origin and its carrier ----
    let adj = adjacency(&ctx, bench, &wiring);
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
                    .and_then(|id| ctx.part(&id.part));
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
            (Some(o), Some(c)) if !joined(&adj, &o.id, &c.id) => format!(
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
fn full_why(ctx: &Ctx, wiring: &Wiring, end: &str) -> String {
    let on: Vec<&Wire> = wiring.at(end).collect();
    match on.as_slice() {
        [one] => format!("{} already goes to {}", ctx.name(end), ctx.name(one.other(end))),
        _ => format!("{} already has {} cables, the most it takes", ctx.name(end), on.len()),
    }
}

/// The parts mounted on each board, by the board's id; a part mounted on
/// itself is a `mount-cycle`, never a part its own headers carry. One map
/// for the check's `mount-full` and the mount verdicts, so the two counts
/// cannot drift.
fn mounts_by_board(bench: &Bench) -> BTreeMap<&str, Vec<&Part>> {
    let mut on: BTreeMap<&str, Vec<&Part>> = BTreeMap::new();
    for p in &bench.parts {
        if let Some(base) = p.mounted_on.as_deref().filter(|b| *b != p.id) {
            on.entry(base).or_default().push(p);
        }
    }
    on
}

/// How many parts a template's stack sockets take.
fn stack_room(t: &PartTemplate) -> usize {
    t.connectors
        .iter()
        .filter(|c| c.kind == Kind::StackSocket)
        .fold(0u32, |sum, c| sum.saturating_add(c.max_cables)) as usize
}

/// Where a part's stack plug may be mounted: one verdict per stack socket on
/// another part, every other end hidden, so a shield dragged onto a board is
/// drawn by the server's word like a cable being laid (phase 3).
///
/// A socket refuses when its board already carries what its sockets take —
/// counting every part mounted there but the one being placed, which is
/// moving, and never a board mounted on itself — and when its board sits on
/// the part being placed, directly or up a stack, since a stack has a board
/// at the bottom (`mount-cycle`). Nothing else is refused here: a missing
/// socket or plug is a finding, not a verdict.
///
/// **One pass per request** (fix round 1): the mount map is built once, what
/// sits on the placed part is found by walking down from it, and each board
/// gets one verdict, copied to each of its sockets — a 500-part stack costs
/// a few map lookups per part, not a walk per socket.
fn mount_candidates(ctx: &Ctx, bench: &Bench, placing: &Part) -> BTreeMap<String, Verdict> {
    let on = mounts_by_board(bench);
    // Everything sitting on the placed part, however high: mounting it on
    // any of them would make a ring.
    let mut above: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([placing.id.as_str()]);
    while let Some(id) = queue.pop_front() {
        for q in on.get(id).into_iter().flatten() {
            if q.id != placing.id && above.insert(q.id.as_str()) {
                queue.push_back(q.id.as_str());
            }
        }
    }
    let mut verdicts = BTreeMap::new();
    for part in &bench.parts {
        let Some(template) = ctx.template(part) else { continue };
        if template.hub {
            verdicts.insert(part.id.clone(), Verdict::of(Fit::Hidden));
            continue;
        }
        let sockets: Vec<&Connector> = template.connectors.iter().filter(|c| c.kind == Kind::StackSocket).collect();
        let verdict = if sockets.is_empty() || part.id == placing.id {
            None
        } else if above.contains(part.id.as_str()) {
            Some(Verdict::refused(format!(
                "{} sits on {}; a stack has a board at the bottom",
                part.display(),
                placing.display()
            )))
        } else {
            let room = stack_room(template);
            let carried: Vec<&Part> = on
                .get(part.id.as_str())
                .into_iter()
                .flatten()
                .copied()
                .filter(|q| q.id != placing.id)
                .collect();
            Some(if carried.len() >= room {
                Verdict::refused(match carried.as_slice() {
                    [one] => format!("{} already carries {}", part.display(), one.display()),
                    _ => format!("{} already carries {} parts and takes {room}", part.display(), carried.len()),
                })
            } else {
                Verdict::of(Fit::Ok)
            })
        };
        for c in &template.connectors {
            let end = format!("{}.{}", part.id, c.id);
            let v = match &verdict {
                Some(v) if c.kind == Kind::StackSocket => v.clone(),
                _ => Verdict::of(Fit::Hidden),
            };
            verdicts.insert(end, v);
        }
    }
    verdicts
}

/// One verdict for every connector end on the bench, for a cable being laid
/// from `from`, so the browser decides nothing. `Err` when `from` is not an
/// end a cable can start at.
///
/// **A stack plug lays no cable**: from one, the verdicts are where its part
/// may be mounted instead ([`mount_candidates`]) — every stack socket on
/// another part, ok or refused, and everything else hidden.
pub fn candidates(bench: &Bench, templates: &Templates, config: Option<&Config>, from: &str) -> Result<BTreeMap<String, Verdict>, String> {
    let ctx = Ctx::new(bench, templates);
    let wiring = Wiring::new(bench, config);
    let from_end = ctx.resolve(from);
    match &from_end {
        End::Hub(_) | End::Conn(..) => {}
        End::Unknowable(p) => return Err(format!("`{from}`: {}'s template `{}` is missing, so its connectors are unknown", p.display(), p.template)),
        End::Bad(why) => return Err(why.clone()),
    }
    if let End::Conn(placing, c) = &from_end {
        if c.kind == Kind::StackPlug {
            return Ok(mount_candidates(&ctx, bench, placing));
        }
    }
    let buses = Buses::new(&ctx, bench, &wiring);
    let from_full = match &from_end {
        End::Conn(_, c) => wiring.count(from) >= c.max_cables as usize,
        _ => false,
    };

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
            if from_full {
                break 'v Verdict::refused(full_why(&ctx, &wiring, from));
            }
            if let End::Conn(_, c) = &to_end {
                if wiring.count(&to) >= c.max_cables as usize {
                    break 'v Verdict::refused(full_why(&ctx, &wiring, &to));
                }
            }
            if wiring.joins_pair(from, &to) {
                break 'v Verdict::refused(format!("{} and {} are already joined", ctx.name(from), ctx.name(&to)));
            }
            if let (End::Conn(_, x), End::Conn(_, y)) = (&from_end, &to_end) {
                if carries_power(x.kind) && carries_power(y.kind) {
                    let mut merged: BTreeSet<&String> = buses.group_of(from).iter().collect();
                    merged.extend(buses.group_of(&to));
                    let merged: Vec<String> = merged.into_iter().cloned().collect();
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
            version: None,
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

    /// A config name a save refuses (a hand edit) is an error finding in the
    /// save's own sentence, so the refusal is on screen before Save.
    #[test]
    fn a_config_name_a_save_refuses_is_a_finding_in_its_words() {
        let mut bench = pd_bench();
        bench.configs[0].name = "A ".into();
        let f = check(&bench, &templates(), Some(&bench.configs[1]));
        let refused = f.iter().find(|f| f.code == "config-name-refused").expect("found");
        assert_eq!(refused.level, Level::Error);
        let said = crate::bench::files::check_config_name("A ").unwrap_err();
        assert_eq!(refused.text, format!("{said}. Rename it to save the bench."));
        bench.configs[0].name = "A".into();
        assert!(!codes(&check(&bench, &templates(), Some(&bench.configs[1]))).contains(&"config-name-refused"));
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
    fn a_ring_of_mounts_is_an_error_named_once() {
        let mut bench = pd_bench();
        bench.parts[1].mounted_on = Some("drp".into()); // partner on drp, drp on partner
        let f = check(&bench, &templates(), None);
        let rings: Vec<_> = f.iter().filter(|f| f.code == "mount-cycle").collect();
        assert_eq!(rings.len(), 1, "{f:#?}");
        assert_eq!(rings[0].parts, ["drp", "partner"]);
        assert!(rings[0].text.contains("drp → pd-bench"), "{}", rings[0].text);
        bench.parts[1].mounted_on = Some("partner".into());
        let f = check(&bench, &templates(), None);
        assert!(f.iter().any(|f| f.code == "mount-cycle" && f.text == "pd-bench is mounted on itself."), "{f:#?}");
    }

    /// The ring finder as it was before fix round 2: a walk from every part,
    /// checking its chain at each step. Kept to hold the one walk to it.
    fn mount_cycles_by_walks(ctx: &Ctx, bench: &Bench) -> Vec<Vec<String>> {
        let mut cycles: BTreeSet<Vec<String>> = BTreeSet::new();
        for start in &bench.parts {
            let mut chain: Vec<&str> = vec![start.id.as_str()];
            let mut at = start;
            while let Some(next) = at.mounted_on.as_deref().and_then(|b| ctx.part(b)) {
                if next.id == start.id {
                    let min = chain.iter().enumerate().min_by_key(|(_, id)| **id).map_or(0, |(i, _)| i);
                    chain.rotate_left(min);
                    cycles.insert(chain.iter().map(|s| s.to_string()).collect());
                    break;
                }
                if chain.contains(&next.id.as_str()) || chain.len() > bench.parts.len() {
                    break;
                }
                chain.push(&next.id);
                at = next;
            }
        }
        cycles.into_iter().collect()
    }

    fn mounted(ids_and_bases: &[(&str, Option<&str>)]) -> Bench {
        let parts = ids_and_bases
            .iter()
            .map(|(id, base)| {
                let mut p = part(id, "riser");
                p.mounted_on = base.map(str::to_string);
                p
            })
            .collect();
        Bench { slug: "m".into(), name: "M".into(), legacy: false, version: None, parts, cables: vec![], configs: vec![], signals: vec![] }
    }

    #[test]
    fn rings_are_found_in_one_walk_exactly_as_before() {
        let t = stackable();
        let cases: Vec<Vec<(&str, Option<&str>)>> = vec![
            vec![("a", Some("a"))],                                                     // mounted on itself
            vec![("a", Some("b")), ("b", Some("a"))],                                   // two
            vec![("t", Some("a")), ("a", Some("b")), ("b", Some("c")), ("c", Some("a"))], // a tail into a ring of three
            vec![("a", None), ("b", Some("a")), ("c", Some("b"))],                      // a tower, no ring
            vec![("a", Some("b")), ("b", Some("a")), ("c", Some("d")), ("d", Some("c")), ("e", Some("e"))], // three rings
            vec![("x", Some("y")), ("y", Some("x")), ("x", Some("z")), ("z", None)],     // a second x
            vec![("x", None), ("y", Some("x")), ("x", Some("y"))],                      // a second x closing a ring the first is not in
            vec![("a", Some("nowhere")), ("b", Some("a"))],                             // a mount on no part
        ];
        for parts in &cases {
            let bench = mounted(parts);
            let ctx = Ctx::new(&bench, &t);
            assert_eq!(mount_cycles(&ctx, &bench), mount_cycles_by_walks(&ctx, &bench), "{parts:?}");
        }
        // And on many benches no one wrote by hand: ids reused, mounts on
        // themselves, on nothing, on no part.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut roll = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..400 {
            let n = 1 + roll(12) as usize;
            let names: Vec<String> = (0..n).map(|_| format!("p{}", roll(9))).collect();
            let bases: Vec<Option<String>> = (0..n)
                .map(|_| match roll(5) {
                    0 => None,
                    1 => Some("ghost".to_string()),
                    _ => Some(format!("p{}", roll(9))),
                })
                .collect();
            let pairs: Vec<(&str, Option<&str>)> = names.iter().zip(&bases).map(|(id, b)| (id.as_str(), b.as_deref())).collect();
            let bench = mounted(&pairs);
            let ctx = Ctx::new(&bench, &t);
            assert_eq!(mount_cycles(&ctx, &bench), mount_cycles_by_walks(&ctx, &bench), "{pairs:?}");
        }
    }

    #[test]
    fn a_ring_of_the_part_cap_is_found_cheaply() {
        let t = stackable();
        let n = crate::bench::files::MAX_PARTS;
        let names: Vec<String> = (0..n).map(|i| format!("r{i}")).collect();
        let bases: Vec<String> = (0..n).map(|i| format!("r{}", (i + 1) % n)).collect();
        let pairs: Vec<(&str, Option<&str>)> = names.iter().zip(&bases).map(|(id, b)| (id.as_str(), Some(b.as_str()))).collect();
        let bench = mounted(&pairs);
        let ctx = Ctx::new(&bench, &t);
        let rings = mount_cycles(&ctx, &bench);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), n);
        // The walk per part this replaced took over half a second here in a
        // debug build; one walk takes a few milliseconds. The best of five,
        // against a bound loose enough for a loaded machine.
        let took = best_of_five(|| {
            mount_cycles(&ctx, &bench);
        });
        assert!(took < std::time::Duration::from_millis(250), "{took:?}");
    }

    #[test]
    fn two_bridges_with_no_serial_cannot_be_told_apart() {
        let mut bench = pd_bench();
        for id in ["ch1", "ch2"] {
            let mut bridge = part(id, "usb-uart-bridge");
            bridge.usb.insert("usb".into(), BTreeMap::from([(Function::Vcom, Binding::Port(PortBinding { vid: Some(0x1a86), pid: Some(0x7523), ..Default::default() }))]));
            bench.parts.push(bridge);
        }
        let f = check(&bench, &templates(), None);
        let clash = f.iter().find(|f| f.code == "usb-identity-twice").unwrap();
        assert!(clash.text.contains("USB 1a86:7523 with no serial") && clash.text.contains("cannot be told apart"), "{}", clash.text);
        // An interface that differs tells them apart.
        bench.parts.last_mut().unwrap().usb.get_mut("usb").unwrap().insert(Function::Vcom, Binding::Port(PortBinding { vid: Some(0x1a86), pid: Some(0x7523), interface: Some(2), ..Default::default() }));
        let second_last = bench.parts.len() - 2;
        bench.parts[second_last].usb.get_mut("usb").unwrap().insert(Function::Vcom, Binding::Port(PortBinding { vid: Some(0x1a86), pid: Some(0x7523), interface: Some(0), ..Default::default() }));
        assert!(!codes(&check(&bench, &templates(), None)).contains(&"usb-identity-twice"));
        // VID-only on both, nothing else: still two that cannot be told apart.
        let n = bench.parts.len();
        for i in [n - 1, n - 2] {
            bench.parts[i].usb.get_mut("usb").unwrap().insert(Function::Vcom, Binding::Port(PortBinding { vid: Some(0x1a86), ..Default::default() }));
        }
        let f = check(&bench, &templates(), None);
        let clash = f.iter().find(|f| f.code == "usb-identity-twice").expect("VID-only pair flagged");
        assert!(clash.text.contains("USB vendor 1a86 (no product id)"), "{}", clash.text);
        // One with a serial and one without, same VID:PID: the second matches the first.
        let n = bench.parts.len();
        bench.parts[n - 1].usb.get_mut("usb").unwrap().insert(Function::Vcom, Binding::Port(PortBinding { vid: Some(0x1a86), pid: Some(0x7523), serial: Some("A1".into()), ..Default::default() }));
        bench.parts[n - 2].usb.get_mut("usb").unwrap().insert(Function::Vcom, Binding::Port(PortBinding { vid: Some(0x1a86), pid: Some(0x7523), ..Default::default() }));
        let f = check(&bench, &templates(), None);
        let clash = f.iter().find(|f| f.code == "usb-identity-twice").unwrap();
        assert_eq!(
            clash.text,
            "ch2 · USB binds USB 1a86:7523 with serial A1, and ch1 · USB the same USB 1a86:7523 with no serial and no interface that differs, so a port matching the second also matches the first; give it its serial or an interface."
        );
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
    fn a_stack_plug_is_offered_the_free_stack_sockets() {
        let mut bench = pd_bench();
        bench.parts.push(part("nucleo2", "nucleo-g0b1re"));
        bench.parts.push(part("snk", "x-nucleo-snk1m1"));
        // From the unmounted sink shield: the partner already carries the DRP.
        let v = candidates(&bench, &templates(), None, "snk.stack").unwrap();
        assert_eq!(v["nucleo2.stack"].fit, Fit::Ok);
        assert_eq!(v["partner.stack"], Verdict::refused("pd-bench already carries drp".into()));
        assert_eq!(v["drp.stack"].fit, Fit::Hidden, "a plug is no socket");
        assert_eq!(v["host"].fit, Fit::Hidden);
        assert!(v.iter().filter(|(k, _)| !k.ends_with(".stack")).all(|(_, v)| v.fit == Fit::Hidden));
        // The DRP itself, being moved, does not count against its own board.
        let v = candidates(&bench, &templates(), None, "drp.stack").unwrap();
        assert_eq!(v["partner.stack"].fit, Fit::Ok);
        // A board on the shield's own stack would make a ring.
        bench.parts.iter_mut().find(|p| p.id == "nucleo2").unwrap().mounted_on = Some("snk".into());
        let v = candidates(&bench, &templates(), None, "snk.stack").unwrap();
        assert_eq!(v["nucleo2.stack"], Verdict::refused("nucleo2 sits on snk; a stack has a board at the bottom".into()));
        // A board mounted on itself does not count against its own headers.
        let mut bench = pd_bench();
        bench.parts.push(part("nucleo3", "nucleo-g0b1re"));
        bench.parts.last_mut().unwrap().mounted_on = Some("nucleo3".into());
        bench.parts.push(part("snk", "x-nucleo-snk1m1"));
        let v = candidates(&bench, &templates(), None, "snk.stack").unwrap();
        assert_eq!(v["nucleo3.stack"].fit, Fit::Ok, "{:?}", v["nucleo3.stack"]);
    }

    /// The quickest of five runs of `f`.
    fn best_of_five(mut f: impl FnMut()) -> std::time::Duration {
        (0..5)
            .map(|_| {
                let started = std::time::Instant::now();
                f();
                started.elapsed()
            })
            .min()
            .unwrap_or_default()
    }

    /// A template with headers both ways, so parts can stack into towers.
    fn stackable() -> Templates {
        let mut t = templates();
        let riser: PartTemplate = toml::from_str(
            r#"
id = "riser"
label = "Riser"
category = "shield"
look = "shield"
[[connectors]]
id = "plug"
label = "Plug"
kind = "stack-plug"
side = "top"
[[connectors]]
id = "socket"
label = "Socket"
kind = "stack-socket"
side = "top"
max_cables = 2
"#,
        )
        .unwrap();
        t.insert(riser.id.clone(), riser);
        t
    }

    fn tower(n: usize) -> Bench {
        let mut parts = vec![part("host", "this-machine")];
        for i in 0..n {
            let mut p = part(&format!("r{i}"), "riser");
            if i > 0 {
                p.mounted_on = Some(format!("r{}", i - 1));
                p.at = None;
            }
            parts.push(p);
        }
        Bench { slug: "t".into(), name: "T".into(), legacy: false, version: None, parts, cables: vec![], configs: vec![], signals: vec![] }
    }

    #[test]
    fn a_ring_through_a_stack_is_refused_and_a_full_board_says_what_it_takes() {
        let t = stackable();
        let mut bench = tower(4);
        // r3 sits on r2 on r1 on r0: r0's plug cannot go on any of them.
        let v = candidates(&bench, &t, None, "r0.plug").unwrap();
        for top in ["r1", "r2", "r3"] {
            assert_eq!(v[&format!("{top}.socket")], Verdict::refused(format!("{top} sits on r0; a stack has a board at the bottom")));
        }
        assert_eq!(v["r0.socket"].fit, Fit::Hidden, "its own socket");
        // r3, moved, may go anywhere under it but its own socket.
        let v = candidates(&bench, &t, None, "r3.plug").unwrap();
        assert_eq!(v["r2.socket"].fit, Fit::Ok, "r3 itself does not count against r2");
        // Two more on r1 fill its two places.
        for id in ["x", "y"] {
            let mut p = part(id, "riser");
            p.mounted_on = Some("r1".into());
            bench.parts.push(p);
        }
        let v = candidates(&bench, &t, None, "r3.plug").unwrap();
        assert_eq!(v["r1.socket"], Verdict::refused("r1 already carries 3 parts and takes 2".into()));
    }

    #[test]
    fn mount_verdicts_on_a_bench_at_the_part_cap_stay_cheap() {
        let t = stackable();
        let bench = tower(crate::bench::files::MAX_PARTS - 1);
        let v = candidates(&bench, &t, None, "r0.plug").unwrap();
        assert_eq!(v.values().filter(|v| v.fit == Fit::Refused).count(), crate::bench::files::MAX_PARTS - 2);
        // One walk per request: the per-socket walk this replaced took
        // seconds here in a debug build. The best of five runs, so a loaded
        // machine running the suite in parallel does not fail it.
        let took = best_of_five(|| {
            candidates(&bench, &t, None, "r0.plug").unwrap();
        });
        assert!(took < std::time::Duration::from_millis(500), "{took:?}");
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
