// embarch-ui bench canvas: a saved bench drawn as the hardware on the shelf,
// and edited there.
//
// Phases 2 and 3 of embarch-bench-editor-proposal.md: the Topology tab's
// main surface, with every other card a panel over it. Everything drawn comes
// from the UI server's bench routes (`/api/bench/*`): the vocabulary, the part
// templates and the bench files. A template's `look` picks the drawing and a
// connector's `kind` picks its glyph; a look or kind this file has no drawing
// for is drawn as a plain board or a plain port, never dropped.
//
// **Editing decides nothing in the browser.** A cable being laid is drawn by
// `POST /api/bench/candidates`' verdict for every end — and so is a shield
// dropped on a board, whose stack plug asks the same route where it may go —
// and what the bench breaks comes from `POST /api/bench/check` after every
// edit. Edits stay in the browser, undoable, until Save writes the file with
// `PUT /api/bench/benches/{slug}` naming the `version` it read (owner,
// 2026-11-01); a `409` offers a reload, never an overwrite.
//
// **Making a bench live** (phase 4) is the one way this file reaches Core,
// and only through the UI server's routes: Make live applies what the saved
// bench declares, answers a report — applied, proposed (each confirmed
// through the ordinary enrol), not proposed, and what Core holds that the
// bench does not name, each retracted only by its own click — and a save of
// the live bench declares it again. Which bench is live is the UI's record
// for the repo, never the repo's; switching config tabs never calls Core.
//
// Two things come from app.js as document events. **The snapshot**
// (`embarch:snapshot`) lights a glow: a `usb-host` connector whose bound probe,
// or bound serial port, Core enumerates now; it also fills the tray with what
// is plugged in and no part binds. A snapshot only toggles classes on the
// canvas — the canvas is rebuilt by a gesture or a load, never by a poll, so a
// selection or a hover survives it. **Validate's verdicts**
// (`embarch:verdicts`) are drawn under the part holding the role, sent after a
// pass and when one is dropped, never from the poll. Decision 43's properties
// carry over: every listener lives on the `<svg>`, and what the pointer marked
// is state re-applied on every rebuild rather than waited for.
//
// Zero-build (decision 2): plain JS, one SVG, no library. No interval: the
// one timer is the short debounce before a check, which is not a poll.

(function () {
  "use strict";

  var card = document.getElementById("bench-card");
  if (!card) return;
  var svg = document.getElementById("bench-svg");
  var wrap = document.getElementById("bench-wrap");
  var bodyEl = document.getElementById("bench-body");
  var picker = document.getElementById("bench-pick");
  var stateEl = document.getElementById("bench-state");
  var pathEl = document.getElementById("bench-path");
  var cfgRow = document.getElementById("bench-config-row");
  var cfgEl = document.getElementById("bench-configs");
  var insEl = document.getElementById("bench-inspector");
  var notesEl = document.getElementById("bench-notes");
  var liveEl = document.getElementById("bench-live-state");
  var hintEl = document.getElementById("bench-hint");
  var noticeEl = document.getElementById("bench-notice");
  var editEl = document.getElementById("bench-edit");
  var partBtn = document.getElementById("bench-part-btn");
  var undoBtn = document.getElementById("bench-undo");
  var redoBtn = document.getElementById("bench-redo");
  var dirtyEl = document.getElementById("bench-dirty");
  var saveBtn = document.getElementById("bench-save");
  var checksBtn = document.getElementById("bench-checks-btn");
  var checksCount = document.getElementById("bench-checks-count");
  var checksHead = document.getElementById("bench-checks-head");
  var checksList = document.getElementById("bench-checks-list");
  var binEl = document.getElementById("bench-bin");
  var binList = document.getElementById("bench-bin-list");
  var binClose = document.getElementById("bench-bin-close");
  var trayEl = document.getElementById("bench-tray");
  insEl.tabIndex = -1;
  var liveEl2 = document.getElementById("bench-live");
  var makeLiveBtn = document.getElementById("bench-make-live");
  var stopLiveBtn = document.getElementById("bench-stop-live");
  var liveBtn = document.getElementById("bench-live-btn");
  var declareBtn = document.getElementById("bench-declare-again");
  var retryBtn = document.getElementById("bench-live-retry");
  var wtBtn = document.getElementById("bench-live-wt");
  var liveHead = document.getElementById("bench-live-head");
  var liveBody = document.getElementById("bench-live-body");
  var dlg = {
    box: document.getElementById("bench-dialog"),
    backdrop: document.getElementById("bench-dialog-backdrop"),
    title: document.getElementById("bench-dialog-title"),
    text: document.getElementById("bench-dialog-text"),
    field: document.getElementById("bench-dialog-field"),
    label: document.getElementById("bench-dialog-label"),
    input: document.getElementById("bench-dialog-input"),
    choices: document.getElementById("bench-dialog-choices"),
    error: document.getElementById("bench-dialog-error"),
    ok: document.getElementById("bench-dialog-ok"),
    cancel: document.getElementById("bench-dialog-cancel")
  };

  var ROUTE_VOCAB = "/api/bench/vocabulary";
  var ROUTE_PARTS = "/api/bench/parts";
  var ROUTE_BENCHES = "/api/bench/benches";
  var ROUTE_CHECK = "/api/bench/check";
  var ROUTE_CANDIDATES = "/api/bench/candidates";
  var ROUTE_PICKERS = "/api/topology/pickers";
  var ROUTE_LIVE = "/api/bench/live";
  var ROUTE_RETRACT = "/api/bench/live/retract";
  var ROUTE_ENROLL = "/api/enroll";
  var BENCH_KEY = "embarch-ui.bench";
  var NEW_BENCH = "+new";       // the picker's "New bench…": no slug has a `+`
  var AUTO_CONFIG = "A";        // the config a first part-to-part cable creates (owner)
  var DRAG_PART = "text/x-embarch-part";
  var DRAG_TRAY = "text/x-embarch-tray";

  var HEAD = 46;            // a part's title band, above its first connector row
  var ROW = 24;             // one connector row
  var FOOT = 10;            // below the last row
  var TOP_STEP = 40;        // between two connectors on a part's top edge
  var DEFAULT_WIDTH = 180;  // a part whose template is gone or gives none
  var DUP_OFFSET = 28;      // how far a second part under one id is drawn from the first
  var GRID_COLS = 5, GRID_DX = 220, GRID_DY = 200;   // where unplaced parts go
  var MONO_ADVANCE = 6.6;   // px per character, 11px Plex Mono (0.6 em, decision 42)
  var SANS_ADVANCE = 7.4;   // px per character, 13px Plex Sans semibold, roughly
  var ZOOM_MIN = 0.01, ZOOM_MAX = 2.5, FIT_MAX = 1.4, FIT_PAD = 60, ZOOM_STEP = 1.2;
  var CHECK_DEBOUNCE_MS = 250;  // after an edit, before the bench is checked
  var MOVE_SLOP = 4;        // px a press travels before it is a drag, not a click
  var SNAP_NEAR = 70;       // how close a shield must come to a board's top to stack on it
  var PLACE_STEP = 24;      // each part placed by a click lands this far off the last
  var WHY_ADVANCE = 6.7;    // px per character of the refusal box's 11px mono
  var HUB_HOME = [20, 110]; // where a new bench's "this machine" sits

  function hasOwn(o, k) { return !!o && Object.prototype.hasOwnProperty.call(o, k); }
  function dict() { return Object.create(null); }

  // ---------------------------------------------------------------- state
  var vocab = null;          // GET /api/bench/vocabulary
  var templates = dict();    // template id -> template, GET /api/bench/parts
  var unreadable = dict();   // template id -> why its file would not load
  var partErrors = [];       // the template files that list could not read
  var benchErrors = [];      // the bench files the bench list could not read
  var benchList = [];
  var bench = null;          // the bench on screen, held whole and edited in place: `version` goes back on save
  var model = null;          // bench + the config on screen, resolved into parts and cables
  var cfgIx = 0;
  var sel = null;            // { kind: "part" | "cable", id }
  var hoverPart = null;
  var verdicts = { roles: dict(), boards: dict() };   // as Validate's pass sent them
  var homes = dict();        // part key -> { id, v, label }: the part each verdict is drawn on, fixed when placed
  var verdictDrops = [];     // { id, text }: verdicts an edit took off their part since the pass
  var snap = { seen: false, known: false, portsKnown: false, portsError: null, probes: dict(), probeList: [], ports: [] };
  var view = { x: 0, y: 0, k: 1 };
  var fitted = false;
  var pan = null;
  var cache = null;          // per-render geometry
  var loadSeq = 0, benchSeq = 0;

  var saved = null;          // the bench's content as last read or saved, canonical JSON
  var undoStack = [], redoStack = [];
  var laying = null;         // a cable being laid: { from, partKey, connId, mode, verdicts, … }
  var laySeq = 0;
  var drag = null;           // a part being moved: { key, before, moved, mounts, snap, why, … }
  var dragSeq = 0;
  var findings = [];         // the last check's findings for the bench on screen
  var checkState = "idle";   // idle | pending | ok | error
  var checkError = null, checkSeq = 0, checkTimer = null;
  var trayArm = null;        // a tray item picked by a click, waiting for a connector
  var trayDrag = null;       // a tray item being dragged
  var trayHover = null;      // the connector a tray drag is over (data-conn)
  var pickers = null, pickersError = null, pickersAsked = false;
  var saving = false;
  var placed = 0;
  var previewing = false;    // drawing a template for the part editor, not the bench
  var dlgState = null;
  var lostEdits = null;      // a bench whose unsaved edits a project switch from elsewhere dropped
  var leavePending = null;   // a switch the human said may drop the edits, until it lands or fails
  var saveInFlight = null;   // the PUT on its way, which a leave waits for
  var lastSaveError = null;  // why the last save did not land, for a leave that waited on it
  var leaveParts = false;    // the leave confirmed also drops the part editor's draft, once it lands
  var lastPass;              // the stamp of the Validate pass the verdicts are from
  var benchLoads = 0;        // which bench is on screen: a new number each time one loads or goes
  var live = null;           // GET /api/bench/live's `live`: which bench and config are live for this repo
  var liveState = "unknown"; // unknown | known | unavailable (no such route) | error
  var liveError = null;
  var liveReport = null;     // the last make-live answer: applied, proposals, not_proposed, leftovers
  var liveRows = dict();     // what a row's button last said, by role and probe or by kind and id
  var superseded = null;     // GET's `superseded`: the repo and bench that took the machine after this one
  var wt = null;             // what the last automatic declare said: { ok, text, title }
  var reportSeq = 0;         // which report the rows' answers belong to
  var projectGen = 0;        // bumped on a project switch: an older answer is about the old repo
  var pendingOffer = null;   // a bench opened to be made live: { slug, say } once it loads
  var liveBusy = false;      // a write below is on its way: its buttons wait
  var liveWrites = 0;        // make-live, write-through and Stop answers still on their way
  var liveSeq = 0;           // which write is the newest: an older write's answer is dropped
  var readSeq = 0;           // which read is the newest — a read never cancels a write
  var readDropped = false;   // a read gave way to a write that then did not land
  var liveOpener = null;     // the button that opened the report, when it is not Live report
  var openingLive = false;
  var listWait = null;       // the bench list on its way: resolves once it has answered
  var linkBoard = null;     // a `#topology?board=` link's board, marked on the parts it names
  var linkPanned = false;
  var settledOnce = false;   // a load has finished, bench or no bench

  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }
  function f(v) { return Math.round(v * 10) / 10; }
  function clamp(v, a, b) { return Math.max(a, Math.min(b, v)); }
  function closest(el, s) { return el && el.closest ? el.closest(s) : null; }
  function cut(text, max) {
    text = String(text == null ? "" : text);
    max = Math.max(3, Math.floor(max));
    return text.length > max ? text.slice(0, max - 1) + "…" : text;
  }
  function errText(e) { return (e && e.message) || String(e); }

  // ---------------------------------------------------------------- the vocabulary
  // Read, never restated: which kind reaches a hub, and which kinds are never
  // cabled (a stack socket or plug joins through `mounted_on`, so it is in no
  // `joins` pair and is not the hub's kind).
  function hubKind() { return vocab && vocab.hub_kind; }
  function neverCabled(kind) {
    if (!vocab || (vocab.kinds || []).indexOf(kind) < 0 || kind === vocab.hub_kind) return false;
    return !(vocab.joins || []).some(function (pair) { return pair.indexOf(kind) >= 0; });
  }
  function carries(conn, fn) {
    return (conn && conn.functions || []).some(function (e) { return e && e.function === fn; });
  }
  // The word a probe binding is filed under; every other function a usb-host
  // carries is a serial port of some kind (the glow already reads it so).
  var PROBE = "probe";

  // ---------------------------------------------------------------- bindings
  // A `usb` binding is a USB serial, an interface on the device the
  // connector's probe is (a number), or a table naming any of serial,
  // interface, vid and pid. One reader for all three, one way to say one.
  function bindingSerial(b, deviceSerial) {
    if (typeof b === "string") return b;
    if (typeof b === "number") return deviceSerial || null;
    if (b && typeof b === "object") {
      if (b.serial != null) return String(b.serial);
      return b.interface != null && b.vid == null && b.pid == null ? deviceSerial || null : null;
    }
    return null;
  }
  function bindingInterface(b) {
    if (typeof b === "number") return b;
    return b && typeof b === "object" && b.interface != null ? b.interface : null;
  }
  function hex4(n) { return typeof n === "number" ? "0x" + ("000" + n.toString(16)).slice(-4) : String(n); }
  function bindingText(b) {
    if (typeof b === "string") return b;
    if (typeof b === "number") return "interface " + b;
    if (b && typeof b === "object") {
      var bits = [];
      if (b.serial != null) bits.push(String(b.serial));
      if (b.interface != null) bits.push("interface " + b.interface);
      if (b.vid != null) bits.push("vid " + hex4(b.vid));
      if (b.pid != null) bits.push("pid " + hex4(b.pid));
      return bits.join(" · ") || "nothing named";
    }
    return String(b);
  }
  function connBindings(p, cid) {
    var usb = p.inst.usb;
    var m = hasOwn(usb, cid) ? usb[cid] : null;
    return m && typeof m === "object" ? m : null;
  }
  function deviceSerial(p, cid) {
    var m = connBindings(p, cid);
    return m && hasOwn(m, PROBE) ? bindingSerial(m[PROBE], null) : null;
  }
  function vidPidOnly(b) {
    return !!b && typeof b === "object" && b.serial == null && b.vid != null && b.pid != null;
  }
  // Whether a serial-port binding names this enumerated port: by serial (and
  // interface, when the binding gives one), or for a bridge with no serial by
  // vid and pid.
  function bindingMatchesPort(b, devSerial, pt) {
    if (!pt) return false;
    var iface = bindingInterface(b);
    if (vidPidOnly(b)) {
      return pt.vendor_id === b.vid && pt.product_id === b.pid && (iface == null || pt.interface === iface);
    }
    var serial = bindingSerial(b, devSerial);
    return !!serial && String(pt.serial_number) === serial && (iface == null || pt.interface === iface);
  }

  // Enumerated now, by the latest snapshot: a probe among its probes, anything
  // else among its serial ports — by serial (and interface, when the binding
  // gives one), or for a bridge with no serial by vid and pid, and then only
  // when exactly one port answers: two is ambiguous, so no glow. Ports are
  // known only when Core listed them; nothing is inferred from anything else.
  function bindingLive(fn, b, devSerial) {
    if (!snap.known) return false;
    if (fn !== PROBE && !snap.portsKnown) return false;
    if (fn !== PROBE && vidPidOnly(b)) {
      return snap.ports.filter(function (pt) { return bindingMatchesPort(b, devSerial, pt); }).length === 1;
    }
    var serial = bindingSerial(b, devSerial);
    if (!serial) return false;
    if (fn === PROBE) return !!snap.probes[serial];
    return snap.ports.some(function (pt) { return bindingMatchesPort(b, devSerial, pt); });
  }

  // ---------------------------------------------------------------- the model
  function currentConfig() {
    var cfgs = (bench && bench.configs) || [];
    return cfgs[cfgIx] || null;
  }
  function cfgName() {
    var c = currentConfig();
    return c ? c.name : null;
  }

  function name(p) { return p.inst.label || (p.missing ? p.inst.template : p.tpl.label) || p.id; }

  // A part whose template is gone, or whose file would not load, is drawn,
  // never dropped (decision 44): a plain board under its own id, its
  // connectors whatever its cables name.
  function standIn(inst) {
    return { id: inst.template, label: inst.template, subtitle: null, category: null, look: null,
             source: null, hub: false, width: DEFAULT_WIDTH, min_height: null, rows_top: null,
             connectors: [], passthrough: [] };
  }

  function connOf(c) {
    return { id: c.id, label: c.label || c.id, kind: c.kind, side: c.side, power: c.power,
             functions: c.functions || [], stack: neverCabled(c.kind) };
  }

  function buildModel() {
    var m = { parts: dict(), order: [], cables: [], notes: [] };
    var dupCount = dict();
    (bench.parts || []).forEach(function (inst) {
      inst = inst || {};
      var id = String(inst.id == null ? "" : inst.id);
      var tpl = hasOwn(templates, inst.template) ? templates[inst.template] : null;
      var key = id, n = 1;
      while (m.parts[key]) { n++; key = id + "#" + n; }
      var p = { key: key, id: id, inst: inst, tpl: tpl || standIn(inst), missing: !tpl,
                unreadable: !tpl && hasOwn(unreadable, inst.template) ? unreadable[inst.template] : null,
                dup: n - 1, conns: [], byId: dict() };
      if (n > 1) {
        dupCount[id] = n;
      }
      (p.tpl.connectors || []).forEach(function (c) {
        var conn = connOf(c);
        p.conns.push(conn);
        p.byId[c.id] = conn;
      });
      m.parts[key] = p;
      m.order.push(p);
    });
    Object.keys(dupCount).forEach(function (id) {
      m.notes.push(dupCount[id] + " parts are called " + id + ": the others are drawn offset from the first, and no cable or mount can name them");
    });
    var seen = dict();
    (bench.cables || []).forEach(function (c, i) { addCable(m, c && c.from, c && c.to, "bench", seen, i); });
    var cfg = currentConfig();
    ((cfg && cfg.cables) || []).forEach(function (pair, i) { addCable(m, pair && pair[0], pair && pair[1], "config", seen, i); });
    placeUnplaced(m);
    sideMissing(m);
    hubPorts(m);
    model = m;
  }

  // A cable is named by its two ends, so the same cable keeps its id across
  // configs and a selection follows it, never the cable at the same index.
  function cableId(level, from, to, seen) {
    var id = (level === "bench" ? "b:" : "c:") + [String(from), String(to)].sort().join(" ");
    seen[id] = (seen[id] || 0) + 1;
    return seen[id] > 1 ? id + " #" + seen[id] : id;
  }
  function endsKey(a, b) { return [String(a), String(b)].sort().join("|"); }

  // `ix` is where the cable sits in its list, which is how deleting it finds
  // it; its id is how everything else does.
  function addCable(m, from, to, level, seen, ix) {
    var a = resolveEnd(m, from), b = resolveEnd(m, to);
    if (!a || !b) {
      var gone = !a ? from : to;
      m.notes.push("cable " + from + " ↔ " + to + ": no part " + String(gone).split(".")[0] + " on this bench");
      return;
    }
    m.cables.push({ id: cableId(level, from, to, seen), level: level, a: a, b: b, from: from, to: to, ix: ix });
  }

  // `<part>.<connector>`, or `<part>` alone for a hub. A connector its template
  // does not have is kept as one that says so, so the cable still lands.
  function resolveEnd(m, key) {
    key = String(key == null ? "" : key);
    var dot = key.indexOf(".");
    var pid = dot < 0 ? key : key.slice(0, dot);
    var cid = dot < 0 ? null : key.slice(dot + 1);
    var p = m.parts[pid];
    if (!p) return null;
    if (p.tpl.hub) return { part: p, conn: null, hub: true };
    cid = cid || "?";
    var conn = p.byId[cid];
    if (!conn) {
      conn = { id: cid, label: cid, kind: null, side: null, power: null, functions: [], missing: true };
      p.conns.push(conn);
      p.byId[cid] = conn;
    }
    return { part: p, conn: conn };
  }
  function partOfEnd(end) {
    end = String(end == null ? "" : end);
    var dot = end.indexOf(".");
    return dot < 0 ? end : end.slice(0, dot);
  }

  // A part with no position and no base to sit on goes in a grid under the rest.
  function placeUnplaced(m) {
    var bottom = 0, i = 0;
    m.order.forEach(function (p) { if (p.inst.at) bottom = Math.max(bottom, p.inst.at[1] + 160); });
    m.order.forEach(function (p) {
      if (p.inst.at || (p.inst.mounted_on && m.parts[p.inst.mounted_on] && !p.dup)) return;
      p.fallbackAt = [20 + (i % GRID_COLS) * GRID_DX, bottom + 40 + Math.floor(i / GRID_COLS) * GRID_DY];
      i++;
    });
  }

  // Where a part's file puts it: its own `at`, its place in the grid under the
  // rest, or the `at` of what it is mounted on; a second part under one id
  // sits a little off the first.
  function rawAt(m, p, depth) {
    var at;
    if (p.inst.at) at = p.inst.at;
    else if (p.fallbackAt) at = p.fallbackAt;
    else {
      var base = p.inst.mounted_on && (depth || 0) < 8 ? m.parts[p.inst.mounted_on] : null;
      at = base ? rawAt(m, base, (depth || 0) + 1) : [0, 0];
    }
    return p.dup ? [at[0] + p.dup * DUP_OFFSET, at[1] + p.dup * DUP_OFFSET] : at;
  }

  // An invented connector faces the far end of its first cable.
  function sideMissing(m) {
    m.cables.forEach(function (cb) {
      [[cb.a, cb.b], [cb.b, cb.a]].forEach(function (pair) {
        var end = pair[0], far = pair[1];
        if (!end.conn || !end.conn.missing || end.conn.side) return;
        end.conn.side = rawAt(m, far.part)[0] >= rawAt(m, end.part)[0] ? "right" : "left";
      });
    });
  }

  // **This machine is a hub**: it has no ports of its own, one per cable that
  // reaches it, named by what it carries — the part at the far end, with the
  // connector when two cables come from one part, and "→ <part>" when that
  // connector carries a probe whose SWD lead reaches another part. Ordered top
  // to bottom by the far part so the cables do not cross.
  function hubPorts(m) {
    var ports = [], fromPart = dict();
    m.cables.forEach(function (cb) {
      [[cb.a, cb.b], [cb.b, cb.a]].forEach(function (pair) {
        if (pair[0].hub && !pair[1].hub) fromPart[pair[1].part.key] = (fromPart[pair[1].part.key] || 0) + 1;
      });
    });
    m.cables.forEach(function (cb) {
      [[cb.a, cb.b], [cb.b, cb.a]].forEach(function (pair) {
        var end = pair[0], far = pair[1];
        if (!end.hub) return;
        var port = { id: "~" + cb.id, label: hubLabel(m, far, fromPart[far.part.key] > 1), kind: hubKind(), side: "right",
                     hubPort: true, cable: cb, far: far, functions: [] };
        end.conn = port;
        ports.push({ hub: end.part, port: port });
      });
    });
    ports.sort(function (x, y) {
      var a = rawAt(m, x.port.far.part), b = rawAt(m, y.port.far.part);
      return a[1] - b[1] || a[0] - b[0];
    });
    ports.forEach(function (e) { e.hub.conns.push(e.port); e.hub.byId[e.port.id] = e.port; });
  }

  function hubLabel(m, far, several) {
    if (far.hub) return name(far.part);
    var label = name(far.part) + (several && far.conn ? " · " + far.conn.label : "");
    var reach = probeReach(m, far.part, far.conn);
    return reach ? label + " → " + name(reach) : label;
  }

  // The part a probe reaches: the connector that reaches this machine carries
  // the probe, and the same part's SWD lead goes to another part.
  function probeReach(m, p, conn) {
    if (!carries(conn, PROBE)) return null;
    var reach = null;
    m.cables.some(function (cb) {
      var mine = cb.a.part === p && !cb.a.hub ? cb.a : cb.b.part === p && !cb.b.hub ? cb.b : null;
      if (!mine || !mine.conn || mine.conn.kind !== "swd") return false;
      var other = mine === cb.a ? cb.b : cb.a;
      if (other.part === p || other.hub) return false;
      reach = other.part;
      return true;
    });
    return reach;
  }

  function cablesAt(p, c) {
    return model.cables.filter(function (cb) {
      return (cb.a.part === p && cb.a.conn === c) || (cb.b.part === p && cb.b.conn === c);
    });
  }
  function otherEnd(cb, p, c) {
    return cb.a.part === p && cb.a.conn === c ? cb.b : cb.a;
  }
  function endName(end) {
    if (end.hub) return name(end.part);
    return name(end.part) + " · " + (end.conn ? end.conn.label : "?");
  }
  function cableById(id) {
    return model ? model.cables.filter(function (cb) { return cb.id === id; })[0] || null : null;
  }

  // The connector key whose bindings decide a glow: the connector itself, or
  // for a hub port or a cable, its far end on a part.
  function liveKeyOf(p, c) {
    return connBindings(p, c.id) ? p.key + "." + c.id : null;
  }
  function cableLiveKey(cb) {
    var end = cb.a.hub ? cb.b : cb.b.hub ? cb.a : null;
    return end && end.conn ? liveKeyOf(end.part, end.conn) : null;
  }

  // The name the routes know a connector by: `<part>.<connector>`, or the
  // hub's id alone for any port on it. A second part under one id, and a
  // connector its template lacks, have none — nothing can be laid to them.
  function endOf(p, c) {
    if (!p || !c || p.dup || p.missing || c.missing) return null;
    return p.tpl.hub ? p.id : p.id + "." + c.id;
  }
  function isHubEnd(end) {
    var p = model && String(end).indexOf(".") < 0 ? model.parts[end] : null;
    return !!p && !!p.tpl.hub;
  }

  // ---------------------------------------------------------------- geometry
  function sideOf(p, c) {
    var s = c.side === "right" || c.side === "top" ? c.side : "left";
    if (p.inst.flip && s !== "top") s = s === "left" ? "right" : "left";
    return s;
  }

  function isWired(p, c) { return cablesAt(p, c).length > 0; }

  // A free connector shown without a hover: the one a cable being laid starts
  // at, one the server said it could reach or refused (a hidden one is not
  // drawn), or one a tray item could be bound on.
  function wanted(p, c) {
    if (laying) {
      var end = endOf(p, c);
      if (!end) return false;
      if (p.key === laying.partKey && c.id === laying.connId) return true;
      var v = laying.verdicts ? laying.verdicts[end] : null;
      return !!v && (v.fit === "ok" || v.fit === "refused");
    }
    var item = curTray();
    return !!item && accepts(p, c, item);
  }

  // The tray item being placed, worked out once per drawing.
  function curTray() {
    if (!cache) return trayItem();
    if (!("tray" in cache)) cache.tray = trayItem();
    return cache.tray;
  }

  // While a usb-host cable is laid, the hub shows a "new USB" slot to drop it
  // on — the server's verdict for the hub itself.
  function layingSlot(p) {
    if (!laying || !laying.verdicts || !p.tpl.hub || p.dup || p.missing) return null;
    var v = laying.verdicts[p.id];
    if (!v || (v.fit !== "ok" && v.fit !== "refused")) return null;
    return { id: "~new", label: "new USB", kind: hubKind(), side: "right", slot: true, functions: [] };
  }

  // **A part shows only its wired connectors**; its free ones appear on intent,
  // hovered or selected. Wired rows come first on every side, so revealing the
  // free ones never moves a cable.
  function visibleConns(p) {
    if (cache && cache.vis[p.key]) return cache.vis[p.key];
    var showAll = hoverPart === p.key || (!!sel && sel.kind === "part" && sel.id === p.key);
    var wired = [], free = [];
    p.conns.forEach(function (c) {
      if (previewing) { if (!c.stack) wired.push(c); return; }
      if (isWired(p, c)) wired.push(c);
      else if (!c.stack && (showAll || wanted(p, c))) free.push(c);
    });
    var slot = layingSlot(p);
    if (slot) free.push(slot);
    var out = { list: wired.concat(free), free: free };
    if (cache) cache.vis[p.key] = out;
    return out;
  }

  // A mounted part keeps the height of every row it could show: it sits on its
  // base and grows upward, so a row revealed on it would push the wired ones.
  function heightRows(p) {
    if (!p.inst.mounted_on) return visibleConns(p).list;
    return p.conns.filter(function (c) { return !c.stack || isWired(p, c); });
  }

  function partH(p) {
    var l = 0, r = 0;
    heightRows(p).forEach(function (c) {
      var s = sideOf(p, c);
      if (s === "left") l++; else if (s === "right") r++;
    });
    var rows = Math.max(l, r);
    var h = rows ? (p.tpl.rows_top || HEAD) + rows * ROW + FOOT : HEAD;
    return Math.max(p.tpl.min_height || 0, h);
  }

  function mountedBase(p, depth) {
    return p.inst.mounted_on && !p.dup && (depth || 0) < 8 ? model.parts[p.inst.mounted_on] || null : null;
  }

  function geom(p, depth) {
    if (cache && cache.geo[p.key]) return cache.geo[p.key];
    var w = p.tpl.width || DEFAULT_WIDTH, h = partH(p), g;
    var base = mountedBase(p, depth);
    if (base) {
      var bg = geom(base, (depth || 0) + 1);
      g = { x: bg.x, y: bg.y - h, w: w, h: h };
    } else {
      var at = rawAt(model, p);
      g = { x: at[0], y: at[1], w: w, h: h };
    }
    if (cache) cache.geo[p.key] = g;
    return g;
  }

  function connPos(p, c) {
    var g = geom(p), side = sideOf(p, c);
    var same = visibleConns(p).list.filter(function (x) { return sideOf(p, x) === side; });
    var i = Math.max(0, same.indexOf(c));
    if (side === "top") return { x: g.x + g.w * 0.6 + i * TOP_STEP, y: g.y, side: side };
    var top = p.tpl.rows_top || HEAD;
    return { x: side === "left" ? g.x : g.x + g.w, y: g.y + top + i * ROW + ROW / 2, side: side };
  }

  function cablePath(a, b) {
    var dx = Math.max(50, Math.abs(b.x - a.x) * 0.45);
    var dy = Math.max(50, Math.abs(b.y - a.y) * 0.45);
    function ctl(pt) {
      return pt.side === "left" ? [pt.x - dx, pt.y] : pt.side === "right" ? [pt.x + dx, pt.y] : [pt.x, pt.y - dy];
    }
    var c1 = ctl(a), c2 = ctl(b);
    return "M" + f(a.x) + "," + f(a.y) + " C" + f(c1[0]) + "," + f(c1[1]) + " " + f(c2[0]) + "," + f(c2[1]) +
      " " + f(b.x) + "," + f(b.y);
  }

  // The side with fewer connectors on show, where a look puts its decoration.
  function quietSide(p) {
    var l = 0, r = 0;
    visibleConns(p).list.forEach(function (c) {
      var s = sideOf(p, c);
      if (s === "left") l++; else if (s === "right") r++;
    });
    return r <= l ? "right" : "left";
  }

  // ---------------------------------------------------------------- the looks
  // **A part is drawn as the thing you would look for on a shelf** (owner,
  // 2026-11-01). The template's `look` picks one of these; a look with no
  // drawing here is drawn as a board. Shapes are approximations of the kind of
  // device, never of one vendor's unit.
  function rect(cls, x, y, w, h, rx) {
    return '<rect class="' + cls + '" x="' + f(x) + '" y="' + f(y) + '" width="' + f(w) + '" height="' + f(h) + '"' +
      (rx ? ' rx="' + rx + '"' : "") + "/>";
  }
  function circle(cls, cx, cy, r) {
    return '<circle class="' + cls + '" cx="' + f(cx) + '" cy="' + f(cy) + '" r="' + r + '"/>';
  }
  function shadow(g, rx) { return rect("bn-shadow", g.x + 3, g.y + 5, g.w, g.h, rx); }
  function bodyRect(g, rx, cls) { return rect("bn-body " + cls, g.x, g.y, g.w, g.h, rx); }
  function holes(g) {
    return [[8, 8], [g.w - 8, 8], [8, g.h - 8], [g.w - 8, g.h - 8]].map(function (d) {
      return circle("bn-hole", g.x + d[0], g.y + d[1], 3);
    }).join("");
  }
  function chip(x, y, s) {
    var d = "";
    for (var i = 0; i < 3; i++) {
      var yy = y + 4 + i * ((s - 8) / 2);
      d += "M" + f(x - 3) + "," + f(yy) + " h3 M" + f(x + s) + "," + f(yy) + " h3 ";
    }
    return '<path class="bn-ic-pin" d="' + d + '"/>' + rect("bn-ic", x, y, s, s, 1.5);
  }
  function pinRow(x0, x1, y) {
    var s = '<g class="bn-pinrow">';
    for (var x = x0; x <= x1; x += 6) s += rect("", x, y, 3, 3) + rect("", x, y + 4, 3, 3);
    return s + "</g>";
  }
  function led(cx, cy, r) { return circle("bn-led", cx, cy, r); }
  function rowMid(p, g) {
    var top = p.tpl.rows_top || HEAD;
    return g.y + top + (g.h - top - FOOT) / 2;
  }
  function hasRows(p, g) { return g.h > (p.tpl.rows_top || HEAD) + FOOT; }
  function pcb(p, g, cls) {
    return shadow(g, 5) + bodyRect(g, 5, cls) + (g.h >= 40 ? holes(g) : "");
  }
  var PCB_LABEL = { tx: 20, text: "bn-silk", inset: 18, labelCls: "bn-silk", trace: true };
  function onPcb(svgText, extra) {
    var out = { svg: svgText };
    for (var k in PCB_LABEL) out[k] = PCB_LABEL[k];
    for (var e in extra || {}) out[e] = extra[e];
    return out;
  }

  var LOOKS = {
    // This machine: a monitor whose screen lists what is plugged in.
    monitor: function (p, g) {
      var cx = g.x + g.w / 2, y = g.y, h = g.h;
      var s = rect("bn-shadow", cx - 31, y + h + 13, 68, 6, 3) + shadow(g, 8) +
        rect("bn-case", cx - 9, y + h - 2, 18, 13) + rect("bn-case", cx - 34, y + h + 10, 68, 6, 3) +
        bodyRect(g, 8, "bn-case") + rect("bn-screen", g.x + 7, y + 7, g.w - 14, h - 14, 4) +
        led(g.x + g.w - 12, y + h - 3.5, 1.8);
      return { svg: s, tx: 16, text: "bn-screen-text", inset: 16, labelCls: "bn-screen-text", below: 18 };
    },
    // A Nucleo-64: the snap-off ST-LINK strip on top, header pins along the bottom.
    nucleo64: function (p, g) {
      var s = pcb(p, g, "bn-pcb") +
        '<line class="bn-perf" x1="' + f(g.x + 4) + '" y1="' + f(g.y + HEAD - 6) + '" x2="' + f(g.x + g.w - 4) + '" y2="' + f(g.y + HEAD - 6) + '"/>';
      if (hasRows(p, g)) s += chip(quietSide(p) === "right" ? g.x + g.w - 44 : g.x + 22, rowMid(p, g) - 10, 20);
      s += pinRow(g.x + 22, g.x + g.w - 26, g.y + g.h - 10);
      return onPcb(s);
    },
    shield: function (p, g) {
      var s = pcb(p, g, "bn-pcb-2");
      if (hasRows(p, g)) s += chip(quietSide(p) === "right" ? g.x + g.w - 40 : g.x + 22, rowMid(p, g) - 9, 18);
      return onPcb(s);
    },
    // A development kit: a PCB antenna meander in the top corner.
    devkit: function (p, g) {
      var ax = g.x + g.w - 50, d = "M" + f(ax) + "," + f(g.y + 40);
      for (var i = 0; i < 4; i++) d += " v-12 h4 v12 h4";
      var s = pcb(p, g, "bn-pcb") + '<path class="bn-antenna-trace" d="' + d + '"/>';
      if (hasRows(p, g)) s += chip(quietSide(p) === "right" ? g.x + g.w - 44 : g.x + 22, rowMid(p, g) - 10, 20);
      return onPcb(s, { subEnd: ax - 6 });
    },
    // A power board: two bulk capacitors and an inductor.
    "power-board": function (p, g) {
      var s = pcb(p, g, "bn-pcb");
      if (hasRows(p, g)) {
        var mid = rowMid(p, g), right = quietSide(p) === "right";
        var capX = right ? g.x + g.w - 30 : g.x + 30, coilX = right ? g.x + g.w - 70 : g.x + 46;
        s += circle("bn-cap", capX, mid - 14, 9) + '<path class="bn-ic-pin" d="M' + f(capX - 6) + "," + f(mid - 14) + ' h12"/>';
        s += circle("bn-cap", capX, mid + 12, 9) + '<path class="bn-ic-pin" d="M' + f(capX - 6) + "," + f(mid + 12) + ' h12"/>';
        s += rect("bn-coil", coilX, mid - 12, 24, 24, 5) + '<circle class="bn-ic-pin" cx="' + f(coilX + 12) + '" cy="' + f(mid) + '" r="6" fill="none"/>';
      }
      return onPcb(s);
    },
    // A bench instrument: a display, a knob and keys, on feet. The display reads
    // the first power role its connectors carry, as served.
    instrument: function (p, g) {
      var role = null;
      p.conns.some(function (c) { if (c.power) role = c.power; return !!c.power; });
      var word = role ? String(role).toUpperCase() : "- - -";
      var left = visibleConns(p).list.some(function (c) { return sideOf(p, c) === "left"; });
      var s = shadow(g, 6) + rect("bn-foot", g.x + 10, g.y + g.h - 1, 16, 5, 1) + rect("bn-foot", g.x + g.w - 26, g.y + g.h - 1, 16, 5, 1) +
        bodyRect(g, 6, "bn-instr") + rect("bn-lcd", g.x + g.w - 62, g.y + 7, 52, 18, 3) +
        '<text class="bn-lcd-text" x="' + f(g.x + g.w - 36) + '" y="' + f(g.y + 20) + '" text-anchor="middle">' + esc(cut(word, 7)) + "</text>";
      if (hasRows(p, g)) {
        var mid = rowMid(p, g), kr = Math.min(11, (g.h - HEAD - FOOT) / 2 - 1);
        var kx = left ? g.x + g.w - 28 : g.x + 28, keyX = left ? kx - 30 : kx + 18;
        s += circle("bn-knob", kx, mid, f(kr)) +
          '<line class="bn-knob-mark" x1="' + f(kx) + '" y1="' + f(mid) + '" x2="' + f(kx + kr * 0.55) + '" y2="' + f(mid - kr * 0.55) + '"/>';
        s += rect("bn-key", keyX, mid - 8, 12, 6, 1.5) + rect("bn-key", keyX, mid + 2, 12, 6, 1.5);
      }
      return { svg: s, tx: 12, text: "", inset: 26, labelCls: "", below: 4, subEnd: g.x + g.w - 68 };
    },
    // An analyzer: a plain small case, nothing of one vendor's colours.
    analyzer: function (p, g) {
      var s = shadow(g, 10) + bodyRect(g, 10, "bn-case") + rect("bn-bezel", g.x + 5, g.y + 5, g.w - 10, g.h - 10, 7) +
        led(g.x + g.w - 18, g.y + g.h - 16, 3);
      return { svg: s, tx: 16, text: "bn-on-case", inset: 22, labelCls: "bn-on-case" };
    },
    // A probe or a bridge: a pod.
    pod: function (p, g) {
      return { svg: shadow(g, 16) + bodyRect(g, 16, "bn-case") + led(g.x + g.w - 18, g.y + 16, 3),
               tx: 14, text: "bn-on-case", inset: 14, labelCls: "bn-on-case", subEnd: g.x + g.w - 26 };
    },
    // A breakout: a small board with one copper bar across it.
    breakout: function (p, g) {
      var s = pcb(p, g, "bn-pcb");
      if (hasRows(p, g)) {
        var mid = rowMid(p, g);
        s += '<line class="bn-trace" x1="' + f(g.x + 28) + '" y1="' + f(mid) + '" x2="' + f(g.x + g.w - 28) + '" y2="' + f(mid) + '"/>';
      }
      return onPcb(s);
    },
    // The generic look: a board. A part with no template to read is a blank
    // outline instead, so what it says about its template stays legible.
    board: function (p, g) {
      if (p.missing) {
        return { svg: shadow(g, 5) + bodyRect(g, 5, "bn-body-missing"), tx: 12, text: "", inset: 14, labelCls: "" };
      }
      var s = pcb(p, g, "bn-pcb");
      if (hasRows(p, g)) s += chip(quietSide(p) === "right" ? g.x + g.w - 44 : g.x + 22, rowMid(p, g) - 10, 20);
      return onPcb(s);
    }
  };
  function lookOf(p) {
    return !p.missing && hasOwn(LOOKS, p.tpl.look) ? p.tpl.look : "board";
  }

  // ---------------------------------------------------------------- the ports
  // A port is drawn as its kind; a kind with no drawing here is a plain port.
  function portGlyph(c, pos, look) {
    var x = pos.x, y = pos.y, top = pos.side === "top", inward = pos.side === "left" ? 1 : -1;
    if (c.missing) return circle("bn-dot", x, y, 5.5);
    switch (c.kind) {
      case "usb-c":
        return top ? rect("bn-dot", x - 8, y - 3.5, 16, 7, 3.5) : rect("bn-dot", x - 3.5, y - 8, 7, 16, 3.5);
      case "usb-host":
        return top ? rect("bn-dot", x - 7, y - 4, 14, 8, 2) : rect("bn-dot", x - 4, y - 7, 8, 14, 1.5);
      case "banana":
        if (look === "instrument") {
          // a red and a black jack; the cable lands on the red one
          return circle("bn-dot", x, y, 6) + circle("bn-jack-red", x, y, 3.2) + circle("bn-jack-black", x + inward * 13, y, 5);
        }
        return circle("bn-dot", x, y, 6) + circle("bn-jack", x, y, 2.2);
      case "terminal":
        return rect("bn-dot", x - 6, y - 9, 12, 18, 2) + circle("bn-screw", x, y - 4, 2.4) + circle("bn-screw", x, y + 4, 2.4);
      case "swd": {
        var s = '<g class="bn-dot">';
        for (var i = 0; i < 2; i++) for (var j = 0; j < 3; j++) s += rect("", x - 4.5 + i * 5.5, y - 7.5 + j * 5.5, 3.5, 3.5, 0.6);
        return s + "</g>";
      }
      case "pin-header": {
        var h = '<g class="bn-dot">';
        for (var k = 0; k < 4; k++) h += rect("", x - 1.75, y - 10 + k * 5.5, 3.5, 3.5, 0.6);
        return h + "</g>";
      }
      case "radio":
        return circle("bn-dot", x, y, 3.5) +
          '<path class="bn-antenna" d="M' + f(x - 5) + "," + f(y - 7) + " a7,7 0 0 1 10,0 M" + f(x - 8) + "," + f(y - 10) + ' a11,11 0 0 1 16,0"/>';
      default:
        return circle("bn-dot", x, y, 5.5);
    }
  }

  function labelSVG(c, pos, g, free, deco) {
    var cls = "bn-clabel" + (free ? " bn-free" : "") + (c.missing ? " bn-missing-text" : "");
    var text = c.missing ? c.label + " ?" : c.label;
    if (pos.side === "top") {
      return '<text class="' + cls + ' bn-outside" x="' + f(pos.x + 10) + '" y="' + f(pos.y - 7) + '">' + esc(text) + "</text>";
    }
    var left = pos.side === "left";
    var lx = left ? g.x + deco.inset : g.x + g.w - deco.inset;
    var room = (g.w - deco.inset - 8) / MONO_ADVANCE;
    var trace = deco.trace
      ? '<line class="bn-trace" x1="' + f(left ? g.x + 4 : g.x + g.w - 4) + '" y1="' + f(pos.y) + '" x2="' + f(left ? g.x + 13 : g.x + g.w - 13) + '" y2="' + f(pos.y) + '"/>'
      : "";
    return trace + '<text class="' + cls + (deco.labelCls ? " " + deco.labelCls : "") + '" x="' + f(lx) + '" y="' + f(pos.y + 4) +
      '" text-anchor="' + (left ? "start" : "end") + '">' + esc(cut(text, room)) + "</text>";
  }

  // ---------------------------------------------------------------- drawing

  // ---------------------------------------------------------------- drawing
  function carriedBy(p) {
    return model.order.filter(function (q) { return mountedBase(q) === p; });
  }

  function partSubtitle(p) {
    if (p.unreadable != null) return "template unreadable · " + p.inst.template;
    if (p.missing) return "template missing · " + p.inst.template;
    var shields = carriedBy(p);
    if (shields.length) return p.tpl.label + " + " + shields.map(name).join(" + ");
    if (p.inst.label && p.inst.label !== p.tpl.label) return p.tpl.label;
    return p.tpl.subtitle || "";
  }

  // **A verdict is drawn on the part Validate checked, and only while that
  // part still holds what it was checked as** (fix round 1). Which part that
  // is — the one holding the role, or a bench board's part by the probe it
  // binds — is fixed when the pass's verdicts arrive or the bench is loaded;
  // an edit since never moves one onto a part Validate never saw. An edit
  // that moves the role or the probe takes the verdict off, and a note says so.
  function identityOf(p) {
    if (p.inst.role) return hasOwn(verdicts.roles, p.inst.role) ? "role:" + p.inst.role : null;
    var usb = p.inst.usb || {}, found = null;
    Object.keys(usb).some(function (cid) {
      var serial = deviceSerial(p, cid);
      if (serial && hasOwn(verdicts.boards, serial)) found = "probe:" + serial;
      return !!found;
    });
    return found;
  }
  function verdictById(id) {
    var kind = id.indexOf("role:") === 0 ? "roles" : "boards", key = id.slice(id.indexOf(":") + 1);
    return hasOwn(verdicts[kind], key) ? verdicts[kind][key] : null;
  }
  function holds(p, id) {
    if (id.indexOf("role:") === 0) return p.inst.role === id.slice(5);
    if (p.inst.role) return false;
    var usb = p.inst.usb || {}, serial = id.slice(6);
    return Object.keys(usb).some(function (cid) { return deviceSerial(p, cid) === serial; });
  }
  function identityText(id) {
    return id.indexOf("role:") === 0 ? "the " + id.slice(5) + " role" : "probe " + id.slice(6);
  }

  // `only`: the identities to place (those a new pass changed); the rest
  // keep the part they are on, or stay off it.
  function placeVerdicts(only) {
    if (!model) { homes = dict(); return; }
    var next = dict();
    if (only) {
      Object.keys(homes).forEach(function (key) {
        var h = homes[key];
        if (!only[h.id] && verdictById(h.id)) next[key] = { id: h.id, v: verdictById(h.id), label: h.label };
      });
      verdictDrops = verdictDrops.filter(function (d) { return !only[d.id] && verdictById(d.id); });
    } else {
      verdictDrops = [];
    }
    model.order.forEach(function (p) {
      if (p.dup || hasOwn(next, p.key)) return;
      var id = identityOf(p);
      if (id && (!only || only[id])) next[p.key] = { id: id, v: verdictById(id), label: name(p) };
    });
    homes = next;
  }

  // After an edit: a part that no longer holds what Validate checked it as
  // loses the verdict, and the notes say which and why.
  function reconcileVerdicts() {
    Object.keys(homes).forEach(function (key) {
      var h = homes[key], p = model.parts[key];
      if (p && holds(p, h.id)) return;
      delete homes[key];
      verdictDrops.push({ id: h.id, text: "Validate checked " + h.label + " as " + identityText(h.id) + "; " +
        (p ? "an edit since moved that" : "the part was deleted since") + ", so its verdict is no longer drawn — validate again" });
    });
  }

  function verdictOf(p) {
    var h = hasOwn(homes, p.key) ? homes[p.key] : null;
    return h && holds(p, h.id) ? h.v : null;
  }

  // The part and everything mounted on it, however high the stack goes.
  function stackFrom(p) {
    var out = [p];
    for (var i = 0; i < out.length && out.length < 64; i++) {
      carriedBy(out[i]).forEach(function (q) { if (out.indexOf(q) < 0) out.push(q); });
    }
    return out;
  }

  // Drawn after every part, so no part covers a verdict. Under the part,
  // unless it sits on a base: then above the top of its stack, so the pill
  // covers neither the base's title band nor a shield above it — one pill a
  // row higher for each part above that carries one too.
  function verdictSVG(p) {
    var v = verdictOf(p);
    if (!v) return "";
    var g = geom(p);
    var text = cut(v.text, (g.w + 80) / 6.4);
    var w = text.length * 6.4 + 16, x = g.x + g.w / 2 - w / 2, y;
    if (mountedBase(p)) {
      var stack = stackFrom(p);
      var top = Math.min.apply(null, stack.map(function (q) { return geom(q).y; }));
      var above = stack.filter(function (q) { return q !== p && verdictOf(q); }).length;
      y = top - 24 - above * 22;
    } else {
      y = g.y + g.h + 6 + ((cache && cache.below[p.key]) || 0);
    }
    var status = /^[a-z]+$/.test(String(v.status)) ? v.status : "other";
    return '<g class="bn-verdict bn-v-' + status + '" data-verdict="' + esc(p.key) + '">' + rect("", x, y, w, 18, 9) +
      '<text x="' + f(g.x + g.w / 2) + '" y="' + f(y + 12.5) + '" text-anchor="middle">' + esc(text) + "</text></g>";
  }

  // How a port reads while a cable is laid or a tray item is placed: the
  // server's verdict for its end, or whether the item could bind there.
  function portState(p, c) {
    var end = endOf(p, c), out = { cls: "", why: "" };
    if (laying) {
      if (p.key === laying.partKey && c.id === laying.connId) out.cls = " bn-source";
      else if (laying.verdicts) {
        var v = end ? laying.verdicts[end] : null;
        if (v && v.fit === "ok") out.cls = " bn-ok";
        else if (v && v.fit === "refused") { out.cls = " bn-refused"; out.why = v.why || ""; }
        else out.cls = " bn-dim";
      }
      if (laying.hoverConn === p.key + "." + c.id) out.cls += " bn-target";
      return out;
    }
    var item = curTray();
    if (item) {
      out.cls = accepts(p, c, item) ? " bn-ok bn-tray-ok" : "";
      if (trayHover === p.key + "." + c.id) out.cls += " bn-target";
    }
    return out;
  }

  function partSVG(p) {
    var g = geom(p), look = lookOf(p), vis = visibleConns(p);
    var isSel = !!sel && sel.kind === "part" && sel.id === p.key;
    var deco = LOOKS[look](p, g);
    var s = '<g class="bn-part' + (isSel ? " bn-sel" : "") + (hoverPart === p.key ? " bn-hover" : "") +
      (p.missing ? " bn-part-missing" : "") + (drag && drag.moved && drag.key === p.key ? " bn-dragging" : "") +
      (!previewing && linkBoard != null && linkMatches().indexOf(p.key) >= 0 ? " bn-link-focus" : "") +
      '" data-part="' + esc(p.key) + '" data-look="' + look + '" data-template="' + esc(p.inst.template) + '">';
    // The ring a finding about this part lights (applyFindings toggles it).
    s += rect("bn-flag", g.x - 5, g.y - 5, g.w + 10, g.h + 10, 9).replace("/>", ' data-flag="' + esc(p.key) + '"/>');
    s += deco.svg;
    var tx = g.x + deco.tx;
    var role = p.inst.role ? String(p.inst.role).toUpperCase() : "";
    var pillW = role ? role.length * 6.2 + 12 : 0;
    var nameEnd = g.x + g.w - (role ? pillW + 22 : 10);
    s += '<text class="bn-name ' + deco.text + '" x="' + f(tx) + '" y="' + f(g.y + 19) + '">' +
      esc(cut(name(p), (nameEnd - tx) / SANS_ADVANCE)) + "</text>";
    var subEnd = deco.subEnd || g.x + g.w - 8;
    s += '<text class="bn-sub ' + deco.text + (p.missing ? " bn-missing-text" : "") + '" x="' + f(tx) + '" y="' + f(g.y + 33) + '">' +
      esc(cut(partSubtitle(p), (subEnd - tx) / MONO_ADVANCE)) + "</text>";
    if (role) {
      s += '<g class="bn-role">' + rect("", g.x + g.w - pillW - 16, g.y + 8, pillW, 16, 8) +
        '<text x="' + f(g.x + g.w - pillW / 2 - 16) + '" y="' + f(g.y + 19.5) + '" text-anchor="middle">' + esc(role) + "</text></g>";
    }
    if (mountedBase(p)) {
      var pins = '<g class="bn-pins">';
      for (var i = 0; g.x + 22 + i * 18 + 8 <= g.x + g.w - 14; i++) pins += rect("", g.x + 22 + i * 18, g.y + g.h - 3, 8, 6, 1);
      s += pins + "</g>";
    }
    vis.list.forEach(function (c) {
      var pos = connPos(p, c), free = vis.free.indexOf(c) >= 0;
      var first = cablesAt(p, c)[0];
      var live = c.hubPort ? cableLiveKey(c.cable) : liveKeyOf(p, c);
      var end = endOf(p, c);
      var st = previewing ? { cls: "", why: "" } : portState(p, c);
      var title = c.label + (c.kind ? " · " + c.kind : "") + (c.power ? " · " + c.power : "") +
        (c.missing ? " · not on " + p.tpl.label : "") + (st.why ? " — " + st.why : "");
      s += '<g class="bn-port' + (free ? " bn-free" : " bn-wired") + (c.missing ? " bn-port-missing" : "") + (c.slot ? " bn-slot" : "") + st.cls +
        '" data-conn="' + esc(p.key + "." + c.id) + '" data-pk="' + esc(p.key) + '" data-cid="' + esc(c.id) + '"' +
        (end ? ' data-end="' + esc(end) + '"' : "") + ' data-kind="' + esc(c.kind || "") + '" data-x="' + f(pos.x) + '" data-y="' + f(pos.y) + '"' +
        (first ? ' data-cable-of="' + esc(first.id) + '"' : "") + (c.hubPort ? " data-hub-port" : "") + (c.slot ? " data-slot" : "") +
        (st.why ? ' data-why="' + esc(st.why) + '"' : "") +
        (live ? ' data-live="' + esc(live) + '"' : "") + ">" +
        circle("bn-hit", pos.x, pos.y, 12) + portGlyph(c, pos, look) + "<title>" + esc(title) + "</title></g>";
      s += labelSVG(c, pos, g, free, deco);
      if (live) s += '<circle class="bn-livering" data-live="' + esc(live) + '" cx="' + f(pos.x) + '" cy="' + f(pos.y) + '" r="11"/>';
    });
    if (cache) cache.below[p.key] = deco.below || 0;
    return s + "</g>";
  }

  // A sentence in a red box beside the pointer: why the end under it, or the
  // board a shield is over, refuses.
  function whySVG(x, y, text) {
    text = cut(text, 110);
    var w = text.length * WHY_ADVANCE + 16;
    return '<g class="bn-why" data-why-box>' + rect("", x + 14, y + 12, w, 22, 6) +
      '<text x="' + f(x + 22) + '" y="' + f(y + 27) + '">' + esc(text) + "</text></g>";
  }

  function layingSVG() {
    if (!laying || laying.mode === "press" || laying.x == null) return "";
    var p = model.parts[laying.partKey], c = p ? p.byId[laying.connId] : null;
    if (!p || !c) return "";
    var a = connPos(p, c);
    var s = '<path class="bn-pending" d="' + cablePath(a, { x: laying.x, y: laying.y, side: laying.x < a.x ? "right" : "left" }) + '"/>';
    if (laying.why) s += whySVG(laying.x, laying.y, laying.why);
    return s;
  }

  function dragSVG() {
    if (!drag || !drag.moved) return "";
    var p = model.parts[drag.key];
    if (!p) return "";
    var s = "";
    if (drag.snap && model.parts[drag.snap]) {
      var base = model.parts[drag.snap], bg = geom(base), g = geom(p);
      s += rect("bn-snap-ghost", bg.x, bg.y - g.h, g.w, g.h, 9) +
        '<text class="bn-snap-text" x="' + f(bg.x + g.w / 2) + '" y="' + f(bg.y - g.h - 8) + '" text-anchor="middle">release to stack on ' +
        esc(name(base)) + "</text>";
    } else if (drag.why && drag.x != null) {
      s += whySVG(drag.x, drag.y, drag.why);
    }
    return s;
  }

  function render() {
    if (!bench || !model) return;
    cache = { vis: dict(), geo: dict(), below: dict() };
    var s = '<defs><pattern id="bench-dots" width="24" height="24" patternUnits="userSpaceOnUse">' +
      '<circle class="bn-grid-dot" cx="1" cy="1" r="1"/></pattern></defs>' +
      '<rect id="bench-grid" fill="url(#bench-dots)" x="0" y="0" width="10" height="10"/>';
    model.cables.forEach(function (cb) {
      var d = cablePath(connPos(cb.a.part, cb.a.conn), connPos(cb.b.part, cb.b.conn));
      var kindEnd = cb.a.hub ? cb.b : cb.a;
      var kind = (kindEnd.conn && kindEnd.conn.kind) || "";
      var live = cableLiveKey(cb);
      var isSel = !!sel && sel.kind === "cable" && sel.id === cb.id;
      if (live) s += '<path class="bn-glow" data-live="' + esc(live) + '" d="' + d + '"/>';
      s += '<path class="bn-cable' + (cb.level === "config" ? " bn-cfg" : " bn-bench") + (isSel ? " bn-sel" : "") +
        '" data-cable="' + esc(cb.id) + '" data-ends="' + esc(endsKey(cb.from, cb.to)) + '" data-kind="' + esc(kind) + '" d="' + d + '"/>';
      s += '<path class="bn-cable-hit" data-cable-hit="' + esc(cb.id) + '" d="' + d + '"><title>' +
        esc(endName(cb.a) + " ↔ " + endName(cb.b)) + "</title></path>";
    });
    // Bases first, so a mounted part sits on top of the one it is mounted on.
    var depth = function (p) {
      var n = 0, q = p;
      while (q && mountedBase(q) && n < 8) { q = mountedBase(q); n++; }
      return n;
    };
    model.order.slice().sort(function (a, b) { return depth(a) - depth(b); }).forEach(function (p) { s += partSVG(p); });
    model.order.forEach(function (p) { s += verdictSVG(p); });
    s += dragSVG() + layingSVG();
    svg.innerHTML = s;
    setViewBox();
    applyLive();
    applyFindings();
    renderHint();
  }

  // ---------------------------------------------------------------- the live glow
  // Toggling classes is all a snapshot does here.
  function liveSets() {
    var out = { conns: dict(), bindings: dict() };
    if (!model) return out;
    model.order.forEach(function (p) {
      var usb = p.inst.usb;
      if (!usb || typeof usb !== "object") return;
      Object.keys(usb).forEach(function (cid) {
        var m = connBindings(p, cid);
        if (!m) return;
        var dev = deviceSerial(p, cid);
        Object.keys(m).forEach(function (fn) {
          if (!bindingLive(fn, m[fn], dev)) return;
          out.conns[p.key + "." + cid] = true;
          out.bindings[p.key + "|" + cid + "|" + fn] = true;
        });
      });
    });
    return out;
  }

  function applyLive() {
    var sets = liveSets();
    svg.querySelectorAll("[data-live]").forEach(function (el) {
      el.classList.toggle("bn-live", !!sets.conns[el.getAttribute("data-live")]);
    });
    insEl.querySelectorAll("[data-live-binding]").forEach(function (el) {
      el.classList.toggle("bn-live", !!sets.bindings[el.getAttribute("data-live-binding")]);
    });
    // Core not answering, or not listing its serial ports, is not "nothing
    // plugged in": say so, and those glows stay off.
    var line = !snap.seen || !model ? ""
      : !snap.known ? "Core is not answering: what is plugged in is unknown"
      : !snap.portsKnown ? "Core did not list its serial ports: which are plugged in is unknown" : "";
    liveEl.hidden = !line;
    liveEl.textContent = line;
    liveEl.title = line && snap.known && snap.portsError ? String(snap.portsError) : "";
  }

  document.addEventListener("embarch:snapshot", function (ev) {
    var s = ev.detail || {};
    var probes = dict();
    (s.probes || []).forEach(function (p) { if (p && p.serial_number) probes[String(p.serial_number)] = true; });
    var known = !!s.core_reachable;
    snap = { seen: true, known: known, portsKnown: known && !s.serial_ports_error, portsError: s.serial_ports_error || null,
             probes: probes, probeList: (s.probes || []).filter(function (p) { return p && p.serial_number; }), ports: s.serial_ports || [] };
    applyLive();
    // The tray is not the canvas: it follows what is plugged in, chip by chip,
    // so a chip with focus keeps it. A picked item that was unplugged is put
    // down by toggling classes — a snapshot never rebuilds the canvas — and
    // a tray item being dragged keeps the tray as it is until it lands.
    if (trayArm && !trayItemByKey(trayArm)) {
      trayArm = null;
      svg.querySelectorAll(".bn-tray-ok").forEach(function (el) { el.classList.remove("bn-ok", "bn-tray-ok", "bn-target"); });
      renderHint();
    }
    if (!trayDrag) renderTray();
  });

  // Validate's verdicts, sent after a pass and when one is dropped. A pass is
  // a gesture, not a poll, so the canvas redraws for it.
  // A verdict that changed, or is new, is placed on whichever part holds its
  // role or probe now; one that did not keeps its part, or stays off it.
  document.addEventListener("embarch:verdicts", function (ev) {
    var d = ev.detail || {};
    var next = { roles: dict(), boards: dict() }, changedIds = dict();
    [["roles", "role:"], ["boards", "probe:"]].forEach(function (k) {
      var src = d[k[0]] || {};
      Object.keys(src).forEach(function (id) { next[k[0]][id] = src[id]; });
      Object.keys(next[k[0]]).concat(Object.keys(verdicts[k[0]])).forEach(function (id) {
        if (JSON.stringify(next[k[0]][id]) !== JSON.stringify(verdicts[k[0]][id])) changedIds[k[1] + id] = true;
      });
    });
    verdicts = next;
    // A new pass places every verdict afresh — one that repeats the last
    // pass's result word for word included, since it checked the bench as it
    // stands now; a verdict merely dropped re-places only what changed.
    if (d.pass !== lastPass) { lastPass = d.pass; placeVerdicts(); }
    else placeVerdicts(changedIds);
    render();
    renderNotes();
  });

  // ---------------------------------------------------------------- the checks
  // Re-run after every edit, a load and a config switch, a short debounce
  // after the last one. A check that could not run says so: its findings are
  // unknown, never "nothing to report".
  function scheduleCheck() {
    if (!bench) return;
    clearTimeout(checkTimer);
    var seq = ++checkSeq;
    checkState = "pending";
    renderChecks();
    checkTimer = setTimeout(function () { runCheck(seq); }, CHECK_DEBOUNCE_MS);
  }

  function runCheck(seq) {
    if (!bench || seq !== checkSeq) return;
    send("POST", ROUTE_CHECK, { bench: benchBody(), config: cfgName() }).then(function (r) {
      if (seq !== checkSeq) return;
      findings = (r && r.findings) || [];
      checkState = "ok";
      checkError = null;
      renderChecks();
      applyFindings();
    }).catch(function (e) {
      if (seq !== checkSeq) return;
      findings = [];
      checkState = "error";
      checkError = errText(e);
      renderChecks();
      applyFindings();
    });
  }

  function levelOf(fd) { return fd && (fd.level === "error" || fd.level === "warn" || fd.level === "info") ? fd.level : "info"; }

  function applyFindings() {
    var parts = dict(), cables = dict();
    if (checkState === "ok") {
      findings.forEach(function (fd) {
        var lvl = levelOf(fd) === "error" ? 2 : levelOf(fd) === "warn" ? 1 : 0;
        if (!lvl) return;
        (fd.parts || []).forEach(function (id) { parts[id] = Math.max(parts[id] || 0, lvl); });
        (fd.cables || []).forEach(function (pair) {
          if (!pair) return;
          var k = endsKey(pair[0], pair[1]);
          cables[k] = Math.max(cables[k] || 0, lvl);
        });
      });
    }
    svg.querySelectorAll("[data-flag]").forEach(function (el) {
      var p = model ? model.parts[el.getAttribute("data-flag")] : null;
      var lvl = p && !p.dup && hasOwn(parts, p.id) ? parts[p.id] : 0;
      el.classList.toggle("bn-flag-error", lvl === 2);
      el.classList.toggle("bn-flag-warn", lvl === 1);
    });
    svg.querySelectorAll("path.bn-cable[data-ends]").forEach(function (el) {
      var k = el.getAttribute("data-ends");
      var lvl = hasOwn(cables, k) ? cables[k] : 0;
      el.classList.toggle("bn-f-error", lvl === 2);
      el.classList.toggle("bn-f-warn", lvl === 1);
    });
  }

  function renderChecks() {
    var on = !!bench && !!model;
    checksBtn.hidden = !on;
    if (!on) return;
    var errs = findings.filter(function (fd) { return levelOf(fd) === "error"; }).length;
    var warns = findings.filter(function (fd) { return levelOf(fd) === "warn"; }).length;
    checksBtn.setAttribute("data-state", checkState);
    var worst = checkState !== "ok" ? "unknown" : errs ? "error" : warns ? "warn" : "clean";
    checksBtn.setAttribute("data-worst", worst);
    checksCount.textContent = checkState === "error" ? "?" : checkState === "ok" ? String(errs + warns) : "…";
    checksBtn.title = checkState === "error" ? "The checks could not run: " + checkError
      : checkState !== "ok" ? "checking…"
      : errs + " error" + (errs === 1 ? "" : "s") + ", " + warns + " warning" + (warns === 1 ? "" : "s");
    var scope = cfgName() == null ? "the cables to this machine" : "config " + cfgName();
    checksHead.textContent = "Checks · " + scope;
    if (checkState === "error") {
      checksList.innerHTML = '<p class="sd-error">The checks could not run, so what this bench breaks is unknown: ' + esc(checkError) + "</p>";
      return;
    }
    if (checkState !== "ok") {
      checksList.innerHTML = '<p class="placeholder-note">checking…</p>';
      return;
    }
    if (!findings.length) {
      checksList.innerHTML = '<p class="placeholder-note">Nothing to report for ' + esc(scope) + ".</p>";
      return;
    }
    checksList.innerHTML = '<ul class="bench-checks">' + findings.map(function (fd, i) {
      var lvl = levelOf(fd);
      return '<li class="bench-check bench-check-' + lvl + '" tabindex="0" role="button" data-finding="' + i + '">' +
        '<span class="bench-check-lvl">' + esc(lvl === "warn" ? "warning" : lvl) + "</span><span>" + esc(fd.text) + "</span>" +
        '<span class="bench-check-code">' + esc(fd.code) + "</span></li>";
    }).join("") + "</ul>";
  }

  // A finding selects what it names: its first part, else its first cable.
  function pickFinding(i) {
    var fd = findings[i];
    if (!fd || !model) return;
    var target = null;
    (fd.parts || []).some(function (id) {
      if (model.parts[id]) target = { kind: "part", id: id };
      return !!target;
    });
    if (!target) {
      (fd.cables || []).some(function (pair) {
        var k = pair ? endsKey(pair[0], pair[1]) : null;
        var cb = model.cables.filter(function (c) { return endsKey(c.from, c.to) === k; })[0];
        if (cb) target = { kind: "cable", id: cb.id };
        return !!target;
      });
    }
    if (checksBtn.classList.contains("is-open")) checksBtn.click();
    if (target) select(target);
  }

  checksList.addEventListener("click", function (e) {
    var li = closest(e.target, "[data-finding]");
    if (li) pickFinding(+li.getAttribute("data-finding"));
  });
  checksList.addEventListener("keydown", function (e) {
    var li = closest(e.target, "[data-finding]");
    if (li && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); pickFinding(+li.getAttribute("data-finding")); }
  });

  // ---------------------------------------------------------------- the tray
  // What Core enumerates that no part on this bench binds: a probe by its
  // serial, a serial port by whatever a binding would name it by. Unknown is
  // said, never shown as an empty tray.
  function boundSets() {
    var probes = dict(), portMatchers = [];
    (bench && bench.parts || []).forEach(function (inst) {
      var usb = inst && inst.usb;
      if (!usb || typeof usb !== "object") return;
      Object.keys(usb).forEach(function (cid) {
        var m = usb[cid];
        if (!m || typeof m !== "object") return;
        var dev = hasOwn(m, PROBE) ? bindingSerial(m[PROBE], null) : null;
        Object.keys(m).forEach(function (fn) {
          if (fn === PROBE) {
            var s = bindingSerial(m[fn], null);
            if (s) probes[s] = true;
          } else {
            portMatchers.push({ b: m[fn], dev: dev });
          }
        });
      });
    });
    return { probes: probes, portMatchers: portMatchers };
  }

  function portLabel(pt) {
    var id = pt.serial_number ? String(pt.serial_number)
      : pt.vendor_id != null && pt.product_id != null ? hex4(pt.vendor_id) + ":" + hex4(pt.product_id) : "no serial";
    return (pt.port_name || "port") + " · " + id + (pt.interface != null ? " · if " + pt.interface : "");
  }

  function trayItemsNow() {
    if (!bench || !snap.seen || !snap.known) return null;
    var bound = boundSets(), items = [];
    snap.probeList.forEach(function (pr) {
      var serial = String(pr.serial_number);
      if (bound.probes[serial]) return;
      items.push({ kind: "probe", key: "probe:" + serial, serial: serial,
                   label: (pr.identifier ? pr.identifier + " · " : "") + serial });
    });
    if (snap.portsKnown) {
      snap.ports.forEach(function (pt) {
        if (!pt || !pt.port_name) return;
        if (bound.portMatchers.some(function (mt) { return bindingMatchesPort(mt.b, mt.dev, pt); })) return;
        items.push({ kind: "port", key: "port:" + pt.port_name, port: pt, label: portLabel(pt),
                     dead: !pt.serial_number && (pt.vendor_id == null || pt.product_id == null) });
      });
    }
    return items;
  }

  function trayItemByKey(key) {
    var items = trayItemsNow() || [];
    return items.filter(function (it) { return it.key === key; })[0] || null;
  }
  function trayItem() {
    var key = trayDrag || trayArm;
    return key ? trayItemByKey(key) : null;
  }

  // The functions of this connector the item could be bound as: a probe as
  // the probe, a serial port as any of the others it carries.
  function fnsFor(c, item) {
    if (!c || !item || item.dead) return [];
    var out = [];
    (c.functions || []).forEach(function (e) {
      var fn = e && e.function;
      if (!fn || out.indexOf(fn) >= 0) return;
      if (item.kind === "probe" ? fn === PROBE : fn !== PROBE) out.push(fn);
    });
    return out;
  }
  function accepts(p, c, item) {
    return !!item && !p.dup && !p.missing && !c.missing && !c.hubPort && !c.slot && c.kind === hubKind() && fnsFor(c, item).length > 0;
  }

  // The binding a port is written as: its serial; its serial and interface
  // when siblings share that serial; its vid and pid when it has no serial
  // (with its interface when another serial-less port shares them).
  function portBinding(pt) {
    var serial = pt.serial_number != null && String(pt.serial_number) !== "" ? String(pt.serial_number) : null;
    if (serial) {
      var siblings = snap.ports.filter(function (q) { return q && q !== pt && String(q.serial_number) === serial; });
      return siblings.length && pt.interface != null ? { serial: serial, interface: pt.interface } : serial;
    }
    var b = { vid: pt.vendor_id, pid: pt.product_id };
    var twins = snap.ports.filter(function (q) {
      return q && q !== pt && !q.serial_number && q.vendor_id === pt.vendor_id && q.product_id === pt.product_id;
    });
    if (twins.length && pt.interface != null) b.interface = pt.interface;
    return b;
  }

  // Updated in place, chip by chip, so a snapshot rewrites only what changed
  // and a chip with focus keeps it.
  function fillChip(el, it) {
    var cls = "bench-tray-chip" + (trayArm === it.key ? " is-armed" : "") + (it.dead ? " is-dead" : "");
    if (el.className !== cls) el.className = cls;
    el.draggable = !it.dead;
    var title = it.dead ? "no serial and no VID/PID: nothing could find this port again"
      : "drag onto a usb-host connector, or click it and then the connector";
    if (el.title !== title) el.title = title;
    var text = it.kind + "\u0000" + it.label;
    if (el.getAttribute("data-text") !== text) {
      el.setAttribute("data-text", text);
      el.innerHTML = "<b>" + esc(it.kind) + "</b>" + esc(it.label);
    }
  }

  function renderTray() {
    if (!bench || !model) { trayEl.hidden = true; trayEl.innerHTML = ""; return; }
    trayEl.hidden = false;
    if (!trayEl.querySelector(".bench-tray-items")) {
      trayEl.innerHTML = '<span class="bench-tray-label">Plugged in, on no part</span><span class="bench-tray-items"></span>' +
        '<span class="bench-tray-status"></span>';
    }
    var box = trayEl.querySelector(".bench-tray-items"), status = trayEl.querySelector(".bench-tray-status");
    var items = snap.seen && snap.known ? trayItemsNow() || [] : [];
    var st = !snap.seen ? ["waiting for Core…", "bench-tray-unknown"]
      : !snap.known ? ["Core is not answering: what is plugged in is unknown", "bench-tray-unknown"]
      : !snap.portsKnown ? ["serial ports unknown", "bench-tray-unknown"]
      : !items.length ? ["none", "bench-tray-none"] : ["", ""];
    if (status.textContent !== st[0]) status.textContent = st[0];
    var stCls = "bench-tray-status " + st[1];
    if (status.className !== stCls) status.className = stCls;
    var have = dict(), want = dict();
    Array.prototype.forEach.call(box.children, function (el) { have[el.getAttribute("data-tray")] = el; });
    items.forEach(function (it) { want[it.key] = true; });
    Object.keys(have).forEach(function (k) { if (!want[k]) box.removeChild(have[k]); });
    items.forEach(function (it, i) {
      var el = want[it.key] && have[it.key] && have[it.key].parentNode === box ? have[it.key] : null;
      if (!el) {
        el = document.createElement("span");
        el.setAttribute("role", "button");
        el.tabIndex = 0;
        el.setAttribute("data-tray", it.key);
      }
      fillChip(el, it);
      if (box.children[i] !== el) box.insertBefore(el, box.children[i] || null);
    });
    refit();
  }

  function armTray(key) {
    trayArm = trayArm === key ? null : key;
    laying = null;
    renderTray();
    render();
  }

  function bindTray(item, partKey, connId) {
    var p = model.parts[partKey], c = p ? p.byId[connId] : null;
    if (!p || !c) return;
    var fns = fnsFor(c, item);
    if (!fns.length) return;
    var go = function (fn) {
      var q = model.parts[partKey];
      if (!q) return;
      var b = item.kind === "probe" ? item.serial : portBinding(item.port);
      var replaced = null;
      trayArm = null;
      trayDrag = null;
      trayHover = null;
      edit(function () {
        if (!q.inst.usb || typeof q.inst.usb !== "object") q.inst.usb = {};
        var usb = q.inst.usb;
        if (!usb[connId] || typeof usb[connId] !== "object") usb[connId] = {};
        if (hasOwn(usb[connId], fn)) replaced = bindingText(usb[connId][fn]);
        usb[connId][fn] = b;
      });
      var here = model.parts[partKey], dev = here ? deviceSerial(here, connId) : null;
      // A binding that names more than one port names none of them for sure.
      var matches = item.kind === "probe" ? 0 : snap.ports.filter(function (pt) { return bindingMatchesPort(b, dev, pt); }).length;
      notice(name(q) + " · " + c.label + ": " + fn + " is " + bindingText(b) + (replaced ? ", replacing " + replaced : "") +
        (matches > 1 ? " — it matches " + matches + " enumerated ports, so which one is ambiguous" : "") +
        (here && cablesAt(here, here.byId[connId]).length ? "" : " — this connector has no cable to this machine yet"), matches > 1 ? "error" : "ok");
    };
    if (fns.length === 1) { go(fns[0]); return; }
    var m = connBindings(p, connId) || {};
    dialog({
      title: "Which is " + item.label + "?",
      text: name(p) + " · " + c.label + " carries more than one serial port. Pick the one this port is.",
      choices: fns.map(function (fn) {
        return { value: fn, label: fn + (hasOwn(m, fn) ? " — replaces " + bindingText(m[fn]) : "") };
      }),
      cancel: "Cancel"
    }).then(function (fn) {
      if (fn) go(fn); else { trayArm = null; renderTray(); render(); }
    });
  }

  trayEl.addEventListener("click", function (e) {
    var chip = closest(e.target, "[data-tray]");
    if (chip && !chip.classList.contains("is-dead")) armTray(chip.getAttribute("data-tray"));
  });
  trayEl.addEventListener("keydown", function (e) {
    var chip = closest(e.target, "[data-tray]");
    if (chip && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); if (!chip.classList.contains("is-dead")) armTray(chip.getAttribute("data-tray")); }
  });
  trayEl.addEventListener("dragstart", function (e) {
    var chip = closest(e.target, "[data-tray]");
    if (!chip || chip.classList.contains("is-dead")) { e.preventDefault(); return; }
    trayDrag = chip.getAttribute("data-tray");
    trayArm = null;
    try { e.dataTransfer.setData(DRAG_TRAY, trayDrag); e.dataTransfer.effectAllowed = "link"; } catch (_) { /* no data store */ }
    render();
  });
  trayEl.addEventListener("dragend", function () {
    trayDrag = null;
    trayHover = null;
    renderTray();
    render();
  });

  // ---------------------------------------------------------------- the inspector
  function lineFacts(v) { return typeof v === "string" ? JSON.stringify(v) : String(v); }
  function kindCell(c) {
    var fns = (c.functions || []).map(function (fn) {
      var bits = [fn.function];
      for (var k in fn) if (hasOwn(fn, k) && k !== "function" && fn[k] != null) bits.push(k + " " + lineFacts(fn[k]));
      return bits.join(" ");
    });
    return [c.kind || "?"].concat(c.power ? [c.power] : []).join(" · ") + (fns.length ? " · " + fns.join(", ") : "");
  }

  function reaches(p, c) {
    if (c.stack) {
      var on = carriedBy(p).map(name);
      if (on.length) return on.join(", ");
      var base = mountedBase(p);
      return base ? name(base) : "";
    }
    return cablesAt(p, c).map(function (cb) { return endName(otherEnd(cb, p, c)); }).join(", ");
  }

  // The roles a part may hold are the parser's own words, served.
  function roleOptions(current) {
    var roles = (vocab && vocab.roles) || [];
    var opts = ['<option value="">none</option>'].concat(roles.map(function (r) {
      return '<option value="' + esc(r) + '"' + (r === current ? " selected" : "") + ">" + esc(r) + "</option>";
    }));
    if (current && roles.indexOf(current) < 0) opts.push('<option value="' + esc(current) + '" selected>' + esc(current) + " (not a role this build knows)</option>");
    return opts.join("");
  }

  // The board types a role can hold — the project's catalog for the DUT, the
  // suite's supported pair for the dev bench — read from the same route the
  // Board types panel and the role picker read.
  function loadPickers() {
    if (pickersAsked) return;
    pickersAsked = true;
    api(ROUTE_PICKERS).then(function (r) {
      pickers = r || {};
      pickersError = null;
      renderInspector();
      if (linkBoard != null && model) showLinked();
    }).catch(function (e) {
      pickers = null;
      pickersError = errText(e);
      renderInspector();
      if (linkBoard != null && model) showLinked();   // decided without the labels, then
    });
  }

  // The Board types panel changed the catalog: what the pickers offer is
  // read again the next time a role part is inspected — now, if one is.
  document.addEventListener("embarch:boards-changed", function () {
    pickers = null; pickersError = null; pickersAsked = false;
    if (selPart() && selPart().inst.role) renderInspector();
    if (linkBoard != null && model) loadPickers();   // a link waiting on the labels reads them again
  });

  function comboText(c) {
    return (c.revision ? "rev " + c.revision : "no revision") + " · " + (c.variant ? "variant " + c.variant : "no variant");
  }

  function boardEditor(p) {
    var role = p.inst.role, current = p.inst.board || "";
    if (!pickers && !pickersError) {
      loadPickers();
      return '<span>Board type</span><select class="sd-input mono" data-ed="board" disabled><option>reading…</option></select>';
    }
    if (pickersError) {
      return '<span>Board type</span><span class="sd-error">' + esc(current || "none") + " · the board types could not be read: " + esc(pickersError) + "</span>";
    }
    var list = (pickers && pickers[role]) || [];
    var opts = ['<option value="">none</option>'].concat(list.map(function (o) {
      return '<option value="' + esc(o.board) + '"' + (o.board === current ? " selected" : "") + ">" + esc(o.label || o.board) +
        (o.chip ? " — " + esc(o.chip) : " — no chip: set it in Board types") + "</option>";
    }));
    if (current && !list.some(function (o) { return o.board === current; })) {
      opts.push('<option value="' + esc(current) + '" selected>' + esc(current) + " (not offered for " + esc(role) + ")</option>");
    }
    var h = '<span>Board type</span><select class="sd-input mono" data-ed="board">' + opts.join("") + "</select>";
    // **A combination is the DUT's** (decision 47): a dev-bench board builds
    // the one pairing the suite supports, so it is offered none — and the
    // pin preselected is the DUT row's.
    var builds = pickers && pickers.builds, dutRow = (pickers && pickers.dut || []).filter(function (o) { return o.board === current; })[0];
    if (role === "dut" && current && builds && dutRow) {
      var entry = builds.available && builds.by_board ? builds.by_board[current] : null;
      var combos = entry && entry.combos ? entry.combos : [];
      if (!builds.available) {
        h += '<div class="bn-ed-note">' + esc("this repo's build targets could not be scanned" + (builds.reason ? ": " + builds.reason : "")) + "</div>";
      } else if (combos.length) {
        // **The build combination is the project's, picked from what the
        // scan backs** (decision 47): revision and variant together, never
        // a cross product and never typed. It writes only the pin in
        // embarch/boards.toml; Core is told the board type when the bench
        // is made live, never a revision.
        var pin = dutRow;
        var at = -1;
        combos.forEach(function (c, i) { if ((c.revision || "") === (pin.revision || "") && (c.variant || "") === (pin.variant || "")) at = i; });
        var copts = combos.map(function (c, i) {
          return '<option value="' + i + '" title="' + esc(c.qualifier || "") + '"' + (i === at ? " selected" : "") + ">" + esc(comboText(c)) + "</option>";
        });
        if (at < 0) copts.unshift('<option value="" selected disabled>' + esc(pin.revision || pin.variant ? comboText(pin) + " — pinned, not in the scan" : "not pinned") + "</option>");
        h += '<span>Build as</span><select class="sd-input mono" data-pin="' + esc(current) + '" title="The revision and variant a build of ' +
          esc(current) + ' uses, written to embarch/boards.toml">' + copts.join("") + "</select>";
      } else {
        h += '<div class="bn-ed-note">not in this repo\'s target scan</div>';
      }
    }
    return h;
  }

  // Writes the one row's pin to embarch/boards.toml through the catalog's
  // own upsert: the row as the file holds it, revision and variant changed.
  // No Core call; nothing on the bench changes.
  function pinCombo(board, combo) {
    notice("Pinning " + board + "…", "info");
    api("/api/topology/boards").then(function (cat) {
      var row = ((cat && cat.boards) || []).filter(function (b) { return b && b.name === board; })[0];
      if (!row) throw new Error(board + " is not a row in embarch/boards.toml");
      var body = {};
      Object.keys(row).forEach(function (k) { body[k] = row[k]; });
      body.revision = combo.revision || "";
      body.variant = combo.variant || "";
      return send("POST", "/api/topology/boards", body);
    }).then(function () {
      notice("embarch/boards.toml builds " + board + " as " + comboText(combo) + "; Core hears the board type when the bench is made live", "ok");
      pickers = null; pickersAsked = false;
      renderInspector();
      document.dispatchEvent(new CustomEvent("embarch:boards-pinned", { detail: { board: board } }));
    }).catch(function (e) {
      notice("Not pinned: " + errText(e), "error");
      renderInspector();
    });
  }

  function partInspector(p) {
    var h = '<div class="bn-ins-title">' + esc(name(p)) + "</div>";
    if (p.unreadable != null) {
      h += '<div class="sd-error">template ' + esc(p.inst.template) + " unreadable: " + esc(p.unreadable) + "</div>";
    } else if (p.missing) {
      h += '<div class="sd-error">template ' + esc(p.inst.template) + " missing</div>";
    } else {
      var src = p.tpl.source === "repo" ? "embarch/parts/" + p.tpl.id + ".toml" : "built in";
      h += '<div class="bn-ins-meta">' + esc(p.tpl.label) + " · " + esc(src) + "</div>";
    }
    h += '<div class="bn-ed">';
    h += '<span>Label</span><input class="sd-input" type="text" spellcheck="false" data-ed="label" value="' + esc(p.inst.label || "") +
      '" placeholder="' + esc(p.missing ? p.inst.template : p.tpl.label) + '"/>';
    if (!p.tpl.hub) {
      h += '<span>Role</span><select class="sd-input mono" data-ed="role">' + roleOptions(p.inst.role || "") + "</select>";
      if (p.inst.role) h += boardEditor(p);
    }
    h += "</div>";
    h += '<div class="bn-ed-actions"><button type="button" class="btn" data-act="flip" title="Draw its left connectors on the right (F)">Flip</button>' +
      '<button type="button" class="btn btn-icon-danger" data-act="delete" title="Delete the part, its cables in every config with it (Del)">Delete</button></div>';
    var kv = [];
    if (p.inst.mounted_on) {
      var base = mountedBase(p);
      kv.push(["On", base ? name(base) : p.inst.mounted_on + (p.dup ? "" : " (not on this bench)")]);
    }
    var carried = carriedBy(p).map(name);
    if (carried.length) kv.push(["Carries", carried.join(", ")]);
    var v = verdictOf(p);
    if (v) kv.push(["Validate", v.text]);
    if (kv.length) h += '<dl class="bn-kv">' + kv.map(function (r) { return "<dt>" + esc(r[0]) + "</dt><dd>" + esc(r[1]) + "</dd>"; }).join("") + "</dl>";
    var rows = p.conns.map(function (c) {
      var r = reaches(p, c);
      return '<tr data-conn-row="' + esc(c.id) + '"><td class="bn-k" data-kind="' + esc(c.kind || "") + '">' + esc(c.label) +
        (c.missing ? ' <span class="bn-missing-text">not on the template</span>' : "") + "</td>" +
        '<td class="mono">' + esc(kindCell(c)) + "</td><td>" + (r ? esc(r) : '<span class="bn-muted">—</span>') + "</td></tr>";
    }).join("");
    if (rows) h += '<table class="bn-conns"><tbody>' + rows + "</tbody></table>";
    var usb = p.inst.usb && typeof p.inst.usb === "object" ? p.inst.usb : {};
    Object.keys(usb).forEach(function (cid) {
      var m = connBindings(p, cid), c = p.byId[cid];
      if (!m) return;
      h += '<div class="bn-usb-conn">' + esc(c ? c.label : cid) + "</div>";
      Object.keys(m).forEach(function (fn) {
        h += '<div class="bn-usb" data-live-binding="' + esc(p.key + "|" + cid + "|" + fn) + '"><span>' + esc(fn) + "</span> " +
          '<span class="mono">' + esc(bindingText(m[fn])) + '</span><span class="bn-enum">enumerated</span>' +
          '<button type="button" class="btn btn-tiny bn-unbind" data-act="unbind" data-cid="' + esc(cid) + '" data-fn="' + esc(fn) +
          '" title="Unbind: the file stops naming this ' + esc(fn) + '" aria-label="Unbind ' + esc(fn) + '">&times;</button></div>';
      });
    });
    return h;
  }

  function cableInspector(cb) {
    var kindEnd = cb.a.hub ? cb.b : cb.a;
    var kind = (kindEnd.conn && kindEnd.conn.kind) || "?";
    var cfg = currentConfig();
    var h = '<div class="bn-ins-title">' + esc(kind) + " cable</div>" +
      '<div class="bn-ins-meta">' + esc(endName(cb.a)) + " ↔ " + esc(endName(cb.b)) + "</div>";
    h += '<dl class="bn-kv"><dt>Config</dt><dd>' + esc(cb.level === "bench" ? "every config" : (cfg ? cfg.name : "")) + "</dd></dl>";
    var end = cb.a.hub ? cb.b : cb.b.hub ? cb.a : null;
    var m = end && end.conn ? connBindings(end.part, end.conn.id) : null;
    if (m) {
      Object.keys(m).forEach(function (fn) {
        h += '<div class="bn-usb" data-live-binding="' + esc(end.part.key + "|" + end.conn.id + "|" + fn) + '"><span>' + esc(fn) + "</span> " +
          '<span class="mono">' + esc(bindingText(m[fn])) + '</span><span class="bn-enum">enumerated</span></div>';
      });
    }
    h += '<div class="bn-ed-actions"><button type="button" class="btn btn-icon-danger" data-act="delete-cable" title="Delete the cable (Del)">Delete cable</button></div>';
    return h;
  }

  // What has focus in the inspector, so a rebuild for the same selection —
  // an edit committed, the board types answering, a save — puts it back,
  // with a label half typed and its caret, rather than dropping it (fix
  // round 1).
  var insFor = null;
  function inspectorFocus() {
    var a = document.activeElement;
    if (!a || a === insEl || !insEl.contains(a)) return null;
    var css = ["data-ed", "data-act", "data-cid", "data-fn"].filter(function (n) { return a.hasAttribute(n); })
      .map(function (n) { return "[" + n + '="' + CSS.escape(a.getAttribute(n)) + '"]'; }).join("");
    var typing = a.tagName === "INPUT" && a.defaultValue !== a.value;
    return { css: css, value: typing ? a.value : null, start: typing ? a.selectionStart : null, end: typing ? a.selectionEnd : null };
  }

  // The inspector floats over the canvas only while something is selected.
  function renderInspector() {
    var cb = sel && sel.kind === "cable" ? cableById(sel.id) : null;
    var p = sel && sel.kind === "part" && model ? model.parts[sel.id] : null;
    if (!bench || !model || (!cb && !p)) {
      insEl.innerHTML = "";
      insEl.hidden = true;
      insFor = null;
      return;
    }
    var now = sel.kind + ":" + sel.id;
    var keep = insFor === now ? inspectorFocus() : null;
    insEl.innerHTML = p ? partInspector(p) : cableInspector(cb);
    insEl.hidden = false;
    insFor = now;
    if (keep) {
      var back = keep.css ? insEl.querySelector(keep.css) : null;
      if (back) {
        if (keep.value != null) {
          back.value = keep.value;
          try { back.setSelectionRange(keep.start, keep.end); } catch (_) { /* not a text field */ }
        }
        back.focus({ preventScroll: true });
      } else {
        insEl.focus({ preventScroll: true });   // the control went with what it named; stay in the inspector
      }
    }
    applyLive();
  }

  function selPart() {
    return sel && sel.kind === "part" && model ? model.parts[sel.id] || null : null;
  }

  insEl.addEventListener("change", function (e) {
    var pinOf = e.target && e.target.getAttribute ? e.target.getAttribute("data-pin") : null;
    if (pinOf) {
      var entry = pickers && pickers.builds && pickers.builds.by_board ? pickers.builds.by_board[pinOf] : null;
      var combo = entry && entry.combos ? entry.combos[+e.target.value] : null;
      if (combo) pinCombo(pinOf, combo);
      return;
    }
    var what = e.target && e.target.getAttribute ? e.target.getAttribute("data-ed") : null;
    var p = selPart();
    if (!what || !p) return;
    var v = String(e.target.value == null ? "" : e.target.value);
    edit(function () {
      if (what === "label") p.inst.label = v.trim() ? v.trim() : null;
      else if (what === "role") {
        p.inst.role = v || null;
        if (!v) p.inst.board = null;   // a board type is the role's, and goes with it
      } else if (what === "board") p.inst.board = v || null;
    });
  });
  insEl.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && e.target && e.target.getAttribute && e.target.getAttribute("data-ed") === "label") e.target.blur();
  });
  insEl.addEventListener("click", function (e) {
    var b = closest(e.target, "[data-act]");
    if (!b) return;
    var act = b.getAttribute("data-act");
    if (act === "flip") flipSelection();
    else if (act === "delete" || act === "delete-cable") deleteSelection();
    else if (act === "unbind") unbind(b.getAttribute("data-cid"), b.getAttribute("data-fn"));
  });

  // ---------------------------------------------------------------- the frame
  function renderConfigs() {
    var cfgs = (bench && bench.configs) || [];
    var liveCfg = isLiveBench() ? live.config : undefined;
    cfgEl.innerHTML = cfgs.map(function (c, i) {
      var wired = liveCfg !== undefined && c && c.name === liveCfg;
      return '<button type="button" class="bench-cfg' + (wired ? " bench-cfg-live" : "") + '" role="tab" aria-selected="' + (i === cfgIx) + '" data-cfg="' + i + '"' +
        (wired ? ' data-live-config title="The live wiring: the config this bench was made live with"' : "") + ">" +
        (wired ? '<span class="bench-cfg-dot" aria-hidden="true"></span>' : "") + esc(c && c.name) + "</button>";
    }).join("");
    cfgRow.hidden = !cfgs.length;
    refit();
  }

  function renderNotes() {
    var lines = partErrors.concat(benchErrors).map(function (e) {
      return (e && e.file ? e.file : "a file") + ": " + (e && e.error ? e.error : "unreadable");
    }).concat(model && bench ? model.notes : [])
      .concat(model && bench ? verdictDrops.map(function (d) { return d.text; }) : [])
      .concat(lostEdits ? [lostEdits] : []);
    notesEl.innerHTML = lines.map(function (l) { return "<li>" + esc(l) + "</li>"; }).join("");
    notesEl.hidden = !lines.length;
    refit();
  }

  // A row around the canvas changed height (the notes, the tray, the config
  // tabs, the bar): the canvas resized, so its viewBox follows now, not a
  // frame later when the resize observer runs — a click in between would
  // land on the drawing as it stood (final fixes: the config-tab flake).
  function refit() {
    if (bench && model) setViewBox();
  }

  function showState(text, isError) {
    stateEl.textContent = text;
    stateEl.className = isError ? "sd-error bench-state" : "placeholder-note bench-state";
    stateEl.hidden = !text;
  }

  function renderPath() {
    if (!bench) return;
    pathEl.hidden = false;
    pathEl.textContent = "embarch/topologies/" + (bench.slug || "") + ".toml" +
      (bench.legacy ? " · no [[parts]]: laid out from its dut and dev-bench" : "");
  }

  function renderDirty() {
    var on = !!bench && !!model;
    editEl.hidden = !on;
    var dirty = on && isDirty();
    dirtyEl.hidden = !dirty;
    saveBtn.disabled = !dirty || saving;
    undoBtn.disabled = !on || !undoStack.length;
    redoBtn.disabled = !on || !redoStack.length;
    card.classList.toggle("bench-unsaved", dirty);
    renderLiveBar();
    refit();
  }

  function renderHint() {
    if (!hintEl) return;
    var item = trayItem();
    hintEl.textContent = laying && laying.mode !== "press"
      ? "pick the other end: pulsing connectors fit, red ones are refused (hover one for why) · Esc cancels"
      : item ? "drop or click " + item.label + " on a pulsing USB connector to bind it · Esc cancels"
      : drag && drag.moved ? "drop a shield on a board's headers to stack it · drag it off to unstack"
      : "hover a part for its free connectors · press or drag from one to lay a cable · drag a part to move it · Del deletes · F flips · Ctrl+Z undoes · drag empty space to pan";
  }

  // One line over the canvas: what an edit did that is not on the drawing (a
  // config made for a cable, a refusal, a save). It stays until the next one,
  // or a click on it.
  function notice(text, kind) {
    noticeEl.textContent = text || "";
    noticeEl.className = "bench-notice bench-notice-" + (kind || "info");
    noticeEl.setAttribute("data-kind", kind || "info");
    noticeEl.hidden = !text;
  }
  noticeEl.addEventListener("click", function () { notice("", "info"); });

  // A load finished, with a bench or without: a link waiting on it can say
  // now whether the canvas has what it names.
  function settled() {
    settledOnce = true;
    if (linkBoard != null) document.dispatchEvent(new CustomEvent("embarch:bench-marked", { detail: { count: linkMatches().length } }));
  }

  function clearBench() {
    benchLoads++;
    bench = null; model = null; sel = null; hoverPart = null;
    laying = null; drag = null; trayArm = null; trayDrag = null; trayHover = null;
    saved = null; undoStack = []; redoStack = []; findings = []; checkState = "idle";
    clearTimeout(checkTimer);
    svg.innerHTML = "";
    bodyEl.hidden = true;
    pathEl.hidden = true;
    pathEl.textContent = "";
    notice("", "info");
    renderInspector();
    renderDirty();
    renderChecks();
    renderTray();
  }

  // The routes answered 404: this server has no bench routes. One quiet line —
  // never a banner, and never an empty bench standing in for a real one.
  function showUnavailable() {
    clearBench();
    vocab = null; templates = dict(); unreadable = dict(); partErrors = []; benchErrors = []; benchList = [];
    picker.hidden = true;
    notesEl.hidden = true;
    renderBin();
    showState("bench routes not available on this server", false);
  }

  // No project open (409) is a state, said in the server's words; anything
  // else that refused is shown as what it is, route and all.
  function showFailure(e, keepPicker) {
    clearBench();
    if (!keepPicker) picker.hidden = true;
    if (e && e.status === 409) showState(e.message, false);
    else showState((e && e.route ? e.route + ": " : "") + (e && e.message ? e.message : String(e)), true);
    renderNotes();
    settled();
  }

  function failed(route) {
    return function (r) {
      if (r.ok) return r.json();
      return r.text().then(function (t) {
        var err = new Error(t || "HTTP " + r.status);
        err.status = r.status;
        err.route = route;
        err.code = r.headers.get("x-embarch-refusal");
        throw err;
      });
    };
  }
  function api(route) {
    return fetch(route, { headers: { Accept: "application/json" } }).then(failed(route));
  }
  function send(method, route, body) {
    return fetch(route, {
      method: method,
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body)
    }).then(failed(route));
  }

  function remembered() {
    try { return localStorage.getItem(BENCH_KEY); } catch (_) { return null; }
  }
  function remember(slug) {
    try { localStorage.setItem(BENCH_KEY, slug); } catch (_) { /* per-viewer only */ }
  }

  function takeCatalog(res) {
    templates = dict();
    ((res && res.parts) || []).forEach(function (t) { if (t && t.id != null) templates[t.id] = t; });
    partErrors = (res && res.errors) || [];
    unreadable = dict();
    partErrors.forEach(function (e) { if (e && e.id != null && !hasOwn(templates, e.id)) unreadable[e.id] = e.error || "unreadable"; });
  }

  function fillPicker(onScreen) {
    var opts = benchList.map(function (b) {
      return '<option value="' + esc(b.slug) + '">' + esc(b.name || b.slug) + "</option>";
    });
    if (!benchList.length) opts.push('<option value="" disabled>no saved benches</option>');
    opts.push('<option value="' + NEW_BENCH + '">New bench…</option>');
    picker.innerHTML = opts.join("");
    picker.hidden = false;
    picker.value = onScreen || "";
  }

  // `keep`: a reload of the same repo's list (a save or delete on the Saved
  // benches panel), which keeps the zoom, config and selection of the bench
  // on screen when it is still listed — and keeps its unsaved edits whole,
  // re-reading only the list. A project switch keeps nothing. `want` names
  // the bench to open, as a new bench does.
  function loadAll(keep, want) {
    keep = keep === true;
    var seq = ++loadSeq;
    ++benchSeq;   // a bench still loading from before this answers into nothing
    readLive();
    var listed, wait = new Promise(function (res) { listed = res; });
    listWait = wait;
    var answered = function () { if (listWait === wait) listWait = null; listed(); };
    Promise.all([api(ROUTE_VOCAB), api(ROUTE_PARTS), api(ROUTE_BENCHES)]).then(function (res) {
      if (seq !== loadSeq) { answered(); return; }
      vocab = res[0] || {};
      takeCatalog(res[1]);
      benchErrors = (res[2] && res[2].errors) || [];
      benchList = (res[2] && res[2].benches) || [];
      answered();
      renderBin();
      var onScreen = bench ? bench.slug || picker.value : null;
      if (keep && bench && isDirty()) {
        fillPicker(onScreen);
        buildModel();
        render();
        renderNotes();
        return;
      }
      if (!benchList.length) {
        clearBench();
        fillPicker("");
        pathEl.hidden = false;
        pathEl.textContent = "embarch/topologies/";
        // A list emptied by bench files it could not read is not "no benches".
        showState(benchErrors.length ? "no saved bench could be read" : "no saved benches", false);
        renderNotes();
        settled();
        return;
      }
      // A reload keeps the bench on screen when it is still listed.
      var first = want || onScreen || remembered();
      var slug = benchList.some(function (b) { return b.slug === first; }) ? first : benchList[0].slug;
      fillPicker(slug);
      loadBench(slug, keep && slug === onScreen);
    }).catch(function (e) {
      if (seq !== loadSeq) { answered(); return; }
      leaveDone(false);
      if (e && e.status === 404) showUnavailable();
      else showFailure(e, false);
      answered();
    });
  }

  // Runs `fn` once the bench list on its way, if any, has answered — the
  // newest one, when a reload replaced it meanwhile.
  function whenListed(fn) {
    var w = listWait;
    if (!w) { fn(); return; }
    w.then(function () { whenListed(fn); });
  }

  function loadBench(slug, keep) {
    var seq = ++benchSeq;
    var cfgName0 = cfgName();
    api(ROUTE_BENCHES + "/" + encodeURIComponent(slug)).then(function (b) {
      if (seq !== benchSeq) return;
      keep = !!keep && !!bench && bench.slug === slug;
      bench = b || {};
      hoverPart = null;
      laying = null; drag = null; trayArm = null;
      if (keep) {
        var at = (bench.configs || []).map(function (c) { return c && c.name; }).indexOf(cfgName0);
        cfgIx = at < 0 ? 0 : at;
      } else {
        cfgIx = 0; sel = null; fitted = false;
      }
      // Undo reaches back to this load, and no further.
      saved = snapshotContent();
      undoStack = [];
      redoStack = [];
      benchLoads++;
      remember(slug);
      picker.value = slug;
      leaveLanded();
      lastSaveError = null;
      buildModel();
      placeVerdicts();
      if (sel && !(sel.kind === "part" ? model.parts[sel.id] : cableById(sel.id))) sel = null;
      showState("", false);
      renderPath();
      bodyEl.hidden = false;
      notice("", "info");
      renderConfigs();
      render();
      renderInspector();
      renderNotes();
      renderDirty();
      renderTray();
      scheduleCheck();
      if (!keep) fit();
      if (linkBoard != null) showLinked();
      var offer = pendingOffer;
      pendingOffer = null;
      if (offer && offer.slug === slug) offer.say();
      settled();
    }).catch(function (e) {
      if (seq !== benchSeq) return;
      pendingOffer = null;   // the bench it was for did not open: its own line says why
      // A bench that would not open leaves the one on screen, edits and all,
      // with a line saying why — unless no project is open at all.
      if (bench && model && e && e.status !== 409) {
        leaveDone(false);
        picker.value = bench.slug;
        notice("Could not open embarch/topologies/" + slug + ".toml: " + errText(e), "error");
        return;
      }
      leaveDone(false);
      showFailure(e, true);
    });
  }

  // ---------------------------------------------------------------- the edits
  // Every edit goes through `edit`: what the bench said before it is kept
  // for undo, and the canvas, the inspector, the tray and the checks follow.
  // Content is compared in one canonical spelling, so undoing back to what
  // the file says is "no unsaved changes" again.
  function canon(v) {
    if (Array.isArray(v)) return "[" + v.map(canon).join(",") + "]";
    if (v && typeof v === "object") {
      return "{" + Object.keys(v).sort().map(function (k) { return JSON.stringify(k) + ":" + canon(v[k]); }).join(",") + "}";
    }
    return JSON.stringify(v === undefined ? null : v);
  }
  function contentOf(b) {
    return { name: b.name, parts: b.parts || [], cables: b.cables || [], configs: b.configs || [], signals: b.signals || [] };
  }
  function snapshotContent() { return canon(contentOf(bench)); }
  function restoreContent(text) {
    var c = JSON.parse(text);
    bench.name = c.name;
    bench.parts = c.parts;
    bench.cables = c.cables;
    bench.configs = c.configs;
    bench.signals = c.signals;
  }
  function isDirty() { return !!bench && saved != null && snapshotContent() !== saved; }
  // The bench as the routes take it: the file's shape, field for field.
  function benchBody() {
    var c = contentOf(bench);
    return { slug: bench.slug || "", name: c.name, legacy: !!bench.legacy, version: bench.version == null ? null : bench.version,
             parts: c.parts, cables: c.cables, configs: c.configs, signals: c.signals };
  }

  // The notice spoke about the last edit; an edit that changes the bench
  // clears it, and says its own after.
  function edit(fn) {
    if (!bench) return false;
    // A cable being laid or a part being dragged was answered for the bench
    // before this edit; its verdicts no longer hold (fix round 1). Ended even
    // when the edit turns out to change nothing, and then drawn ended.
    var had = !!(laying || drag || trayArm);
    cancelTransient();
    var before = snapshotContent();
    fn();
    if (snapshotContent() === before) {
      if (had) { render(); renderTray(); }
      return false;
    }
    notice("", "info");
    lostEdits = null;
    undoStack.push(before);
    redoStack = [];
    changed();
    return true;
  }

  // After any change to the bench in the browser: rebuild, redraw, re-check.
  function changed() {
    var cfgs = bench.configs || [];
    if (cfgIx >= cfgs.length) cfgIx = Math.max(0, cfgs.length - 1);
    buildModel();
    reconcileVerdicts();
    if (sel && !(sel.kind === "part" ? model.parts[sel.id] : cableById(sel.id))) sel = null;
    renderConfigs();
    render();
    renderInspector();
    renderNotes();
    renderTray();
    renderDirty();
    scheduleCheck();
  }

  function cancelTransient() {
    laying = null;
    drag = null;
    trayArm = null;
  }

  function undo() {
    if (!bench || !undoStack.length) return;
    cancelTransient();
    notice("", "info");
    redoStack.push(snapshotContent());
    restoreContent(undoStack.pop());
    changed();
  }
  function redo() {
    if (!bench || !redoStack.length) return;
    cancelTransient();
    notice("", "info");
    undoStack.push(snapshotContent());
    restoreContent(redoStack.pop());
    changed();
  }

  // A part id from its template's: the id itself, else the first free
  // `<id>-<n>`.
  function freshId(base) {
    var taken = dict();
    (bench.parts || []).forEach(function (p) { if (p && p.id != null) taken[String(p.id)] = true; });
    if (!taken[base]) return base;
    var n = 2;
    while (taken[base + "-" + n]) n++;
    return base + "-" + n;
  }

  function newInst(tplId, id, at) {
    return { id: id, template: tplId, label: null, at: at ? [Math.round(at[0]), Math.round(at[1])] : null,
             role: null, mounted_on: null, flip: false, board: null, chip: null, usb: {} };
  }

  // Whether the point lies on a part, or just above it where a shield sits.
  function overPart(p, x, y) {
    var g = geom(p);
    return x >= g.x - 10 && x <= g.x + g.w + 10 && y >= g.y - SNAP_NEAR - 40 && y <= g.y + g.h;
  }

  // Place a template. Dropped at a point over a board, a part with a stack
  // plug asks the server where it may be mounted first, and lands mounted
  // when that board is offered.
  function placePart(tplId, at, dropPoint) {
    var t = templates[tplId];
    if (!bench || !t) return;
    var id = freshId(String(t.id));
    var inst = newInst(t.id, id, at);
    var commit = function (base) {
      var made = edit(function () {
        if (base) { inst.mounted_on = base.id; inst.at = null; }
        bench.parts = (bench.parts || []).concat([inst]);
      });
      if (!made) return;
      sel = { kind: "part", id: id };
      render();
      renderInspector();
      notice(base ? t.label + " placed on " + name(base) : "", "ok");
    };
    var stacks = (t.connectors || []).filter(function (c) { return c && neverCabled(c.kind); });
    if (!dropPoint || !stacks.length) { commit(null); return; }
    var trial = benchBody();
    trial.parts = trial.parts.concat([inst]);
    var unasked = null;
    Promise.all(stacks.map(function (c) {
      return send("POST", ROUTE_CANDIDATES, { bench: trial, config: cfgName(), from: id + "." + c.id })
        .catch(function (e) { unasked = errText(e); return null; });
    })).then(function (answers) {
      var base = null;
      answers.forEach(function (r) {
        var v = (r && r.verdicts) || {};
        Object.keys(v).forEach(function (end) {
          if (base || !v[end] || v[end].fit !== "ok") return;
          var q = model.parts[partOfEnd(end)];
          if (q && !q.dup && overPart(q, dropPoint[0], dropPoint[1])) base = q;
        });
      });
      commit(base);
      // "No board offered" and "the server could not be asked" differ.
      if (unasked && !base) notice(t.label + " placed unmounted: where it may sit could not be asked (" + unasked + ")", "error");
    });
  }

  function placeAtCentre(tplId) {
    var t = templates[tplId];
    if (!t || !model) return;
    var r = wrapSize(), n = placed++ % 6;
    var w = t.width || DEFAULT_WIDTH;
    placePart(tplId, [view.x + r.w / view.k / 2 - w / 2 + n * PLACE_STEP, view.y + r.h / view.k / 2 - 60 + n * PLACE_STEP], null);
  }

  // A cable to this machine is bench-level, shared by every config; every
  // other cable goes in the config on screen — and with no config yet, in a
  // new one, "A", which the notice says.
  function layCable(a, b) {
    var hub = isHubEnd(a) ? a : isHubEnd(b) ? b : null;
    var level = hub ? "bench" : "config", created = false;
    var from = hub ? (hub === a ? b : a) : a, to = hub || b;
    edit(function () {
      if (hub) {
        bench.cables = (bench.cables || []).concat([{ from: from, to: to }]);
        return;
      }
      if (!(bench.configs || []).length) {
        bench.configs = [{ name: AUTO_CONFIG, cables: [] }];
        cfgIx = 0;
        created = true;
      }
      var cfg = bench.configs[cfgIx];
      cfg.cables = (cfg.cables || []).concat([[from, to]]);
    });
    var k = endsKey(from, to);
    var made = model.cables.filter(function (cb) { return cb.level === level && endsKey(cb.from, cb.to) === k; }).pop();
    if (made) select({ kind: "cable", id: made.id });
    notice(created ? "This bench had no config, so the cable went into a new config, " + AUTO_CONFIG + "." : "", "info");
  }

  function deleteSelection() {
    if (!sel || !model) return;
    if (sel.kind === "cable") {
      var cb = cableById(sel.id);
      if (!cb) return;
      edit(function () {
        var list = cb.level === "bench" ? bench.cables : (currentConfig() || {}).cables;
        if (list && cb.ix != null && cb.ix < list.length) list.splice(cb.ix, 1);
      });
      return;
    }
    var p = model.parts[sel.id];
    if (!p) return;
    var gone = p.inst, id = p.id;
    // Shields on it are unmounted where they stand.
    var lifted = carriedBy(p).map(function (q) { return { inst: q.inst, g: geom(q) }; });
    var shared = model.order.some(function (q) { return q !== p && q.id === id; });
    edit(function () {
      lifted.forEach(function (u) { u.inst.mounted_on = null; u.inst.at = [Math.round(u.g.x), Math.round(u.g.y)]; });
      bench.parts = (bench.parts || []).filter(function (x) { return x !== gone; });
      if (shared) return;   // another part answers to that id, and its cables are its
      var touches = function (end) { return partOfEnd(end) === id; };
      bench.cables = (bench.cables || []).filter(function (c) { return !(c && (touches(c.from) || touches(c.to))); });
      (bench.configs || []).forEach(function (cfg) {
        if (!cfg) return;
        cfg.cables = (cfg.cables || []).filter(function (pair) { return !(pair && (touches(pair[0]) || touches(pair[1]))); });
      });
    });
  }

  function flipSelection() {
    var p = selPart();
    if (!p) return;
    edit(function () { p.inst.flip = !p.inst.flip; });
  }

  function unbind(cid, fn) {
    var p = selPart();
    if (!p || !cid || !fn) return;
    edit(function () {
      var usb = p.inst.usb;
      if (!usb || !hasOwn(usb, cid) || !usb[cid]) return;
      delete usb[cid][fn];
      if (!Object.keys(usb[cid]).length) delete usb[cid];
    });
  }

  // ---------------------------------------------------------------- laying a cable
  // Press on a free connector: the server answers a verdict for every end,
  // drawn at once. Release on an ok end (drag), or click one (click, then
  // click). A refused end says why, beside the pointer and on release.
  function startLaying(p, c, e) {
    var end = endOf(p, c);
    if (!end) return;
    var w = toWorld(e), seq = ++laySeq;
    laying = { from: end, partKey: p.key, connId: c.id, mode: "press", sx: e.clientX, sy: e.clientY,
               x: w.x, y: w.y, verdicts: null, seq: seq, hoverConn: null, hoverEnd: null, why: null, dropOn: undefined };
    trayArm = null;
    notice("", "info");
    render();
    send("POST", ROUTE_CANDIDATES, { bench: benchBody(), config: cfgName(), from: end }).then(function (r) {
      if (!laying || laying.seq !== seq) return;
      laying.verdicts = (r && r.verdicts) || {};
      if (laying.dropOn !== undefined) { finishAt(laying.dropOn); return; }
      refreshHover();
      render();
    }).catch(function (e2) {
      if (!laying || laying.seq !== seq) return;
      laying = null;
      notice(errText(e2), "error");
      render();
    });
  }

  function refreshHover() {
    if (!laying) return;
    var v = laying.verdicts && laying.hoverEnd ? laying.verdicts[laying.hoverEnd] : null;
    laying.why = v && v.fit === "refused" ? v.why || "refused" : null;
  }

  // `target`: the end a release or a click landed on, or null for none.
  function finishAt(target) {
    if (!laying) return;
    if (!laying.verdicts) { laying.dropOn = target; return; }
    var from = laying.from, v = target ? laying.verdicts[target] : null;
    laying = null;
    if (!target || target === from || !v || v.fit === "hidden") { render(); return; }
    if (v.fit === "refused") { notice(v.why || "refused", "error"); render(); return; }
    layCable(from, target);
  }

  function cancelLaying() {
    if (!laying) return false;
    laying = null;
    render();
    return true;
  }

  function endUnder(e) {
    var el = document.elementFromPoint(e.clientX, e.clientY);
    var port = closest(el, "[data-conn]");
    return port && svg.contains(port) ? port : null;
  }

  // ---------------------------------------------------------------- moving a part
  function startDrag(p, e) {
    var w = toWorld(e), g = geom(p);
    drag = { key: p.key, sx: e.clientX, sy: e.clientY, ox: w.x - g.x, oy: w.y - g.y, moved: false,
             before: snapshotContent(), mounts: null, snap: null, why: null, x: null, y: null, seq: ++dragSeq };
  }

  // Where its stack plug may go, asked once when a drag starts, with the part
  // already off whatever it sat on.
  function askMounts(p) {
    var plugs = p.dup ? [] : p.conns.filter(function (c) { return c.stack && !c.missing; });
    if (!plugs.length) return;
    var seq = drag.seq, body = benchBody(), unasked = null, who = name(p);
    Promise.all(plugs.map(function (c) {
      return send("POST", ROUTE_CANDIDATES, { bench: body, config: cfgName(), from: p.id + "." + c.id })
        .catch(function (e) { unasked = errText(e); return null; });
    })).then(function (answers) {
      if (unasked) notice("Where " + who + " may be stacked could not be asked (" + unasked + "); it will not snap onto a board", "error");
      if (!drag || drag.seq !== seq) return;
      drag.unasked = unasked;
      var merged = dict();
      answers.forEach(function (r) {
        var v = (r && r.verdicts) || {};
        Object.keys(v).forEach(function (end) {
          if (v[end] && (v[end].fit === "ok" || v[end].fit === "refused")) merged[end] = v[end];
        });
      });
      drag.mounts = merged;
      computeSnap();
      render();
    });
  }

  // The board a dragged shield would stack on: one whose socket the server
  // offered, close to where the shield would sit on it. A refused one close
  // by says why instead.
  function computeSnap() {
    drag.snap = null;
    drag.why = null;
    var p = model.parts[drag.key];
    if (!p || !drag.mounts) return;
    var g = geom(p), why = null;
    Object.keys(drag.mounts).some(function (end) {
      var base = model.parts[partOfEnd(end)];
      if (!base || base === p) return false;
      var bg = geom(base);
      var near = Math.abs(g.x - bg.x) < SNAP_NEAR && Math.abs(g.y - (bg.y - g.h)) < SNAP_NEAR;
      var overlap = g.x < bg.x + bg.w && g.x + g.w > bg.x && g.y < bg.y + bg.h && g.y + g.h > bg.y;
      if (!near && !overlap) return false;
      if (drag.mounts[end].fit === "ok") { drag.snap = base.key; return true; }
      why = why || drag.mounts[end].why || "refused";
      return false;
    });
    if (!drag.snap) drag.why = why;
  }

  function moveDrag(e) {
    var p = model.parts[drag.key];
    if (!p) { drag = null; return; }
    if (!drag.moved) {
      if (Math.abs(e.clientX - drag.sx) + Math.abs(e.clientY - drag.sy) < MOVE_SLOP) return;
      drag.moved = true;
      if (p.inst.mounted_on) {
        var g = geom(p);
        p.inst.mounted_on = null;
        p.inst.at = [Math.round(g.x), Math.round(g.y)];
      }
      askMounts(p);
    }
    var w = toWorld(e);
    p.inst.at = [Math.round(w.x - drag.ox), Math.round(w.y - drag.oy)];
    drag.x = w.x;
    drag.y = w.y;
    buildModel();
    cache = { vis: dict(), geo: dict(), below: dict() };
    computeSnap();
    render();
  }

  function endDrag() {
    var d = drag;
    drag = null;
    if (!d.moved) { render(); return; }
    var p = model.parts[d.key], base = d.snap ? model.parts[d.snap] : null;
    if (p && base) { p.inst.mounted_on = base.id; p.inst.at = null; }
    if (snapshotContent() !== d.before) { undoStack.push(d.before); redoStack = []; }
    if (p && base) notice(name(p) + " is stacked on " + name(base), "ok");
    else if (!d.unasked) notice("", "info");
    changed();
  }

  // ---------------------------------------------------------------- the parts bin
  function chipsOf(t) {
    if (t.hub) return '<span class="bench-chip">a port per cable</span>';
    var conns = (t.connectors || []).filter(function (c) { return c && !neverCabled(c.kind); });
    var shown = conns.slice(0, 4).map(function (c) {
      return '<span class="bench-chip" data-kind="' + esc(c.kind) + '">' + esc(c.label || c.id) + "</span>";
    }).join("");
    return shown + (conns.length > 4 ? '<span class="bench-chip">+' + (conns.length - 4) + "</span>" : "");
  }

  function binCard(t) {
    return '<div class="bench-bin-card" role="button" tabindex="0" draggable="true" data-template="' + esc(t.id) +
      '" title="Drag onto the bench, or click to place it">' +
      '<span class="bench-bin-name">' + esc(t.label || t.id) + '</span><span class="bench-bin-sub">' + esc(t.subtitle || t.category || "") +
      '</span><span class="bench-chips">' + chipsOf(t) + "</span>" +
      (t.source === "repo" ? '<button type="button" class="btn btn-tiny" data-edit-part="' + esc(t.id) + '" title="Edit embarch/parts/' + esc(t.id) + '.toml">Edit</button>' : "") +
      "</div>";
  }

  function renderBin() {
    var all = Object.keys(templates).map(function (id) { return templates[id]; });
    var built = all.filter(function (t) { return t.source !== "repo"; });
    var repo = all.filter(function (t) { return t.source === "repo"; });
    var bad = partErrors.filter(function (e) { return e && e.id != null; });
    binList.innerHTML = '<div class="bench-bin-head">Built in</div>' + built.map(binCard).join("") +
      '<div class="bench-bin-head">This repo · embarch/parts/</div>' +
      (repo.length ? repo.map(binCard).join("") : bad.length ? "" : '<p class="bn-muted">none yet</p>') +
      bad.map(function (e) {
        return '<div class="bench-bin-card" data-unreadable="' + esc(e.id) + '"><span class="bench-bin-name">' + esc(e.id) +
          '</span><span class="bench-bin-err">' + esc(e.error || "unreadable") + "</span>" +
          '<button type="button" class="btn btn-tiny" data-edit-part="' + esc(e.id) + '">Edit</button></div>';
      }).join("") +
      '<button type="button" class="btn bench-bin-new" data-edit-part="">New part…</button>';
  }

  function setBin(open) {
    binEl.hidden = !open;
    partBtn.setAttribute("aria-expanded", open ? "true" : "false");
    partBtn.classList.toggle("is-open", open);
  }
  partBtn.addEventListener("click", function () { setBin(binEl.hidden); });
  binClose.addEventListener("click", function () { setBin(false); partBtn.focus(); });

  // The part editor (bench-parts.js) opens on this; a null id is a new part.
  function editPart(id) {
    document.dispatchEvent(new CustomEvent("embarch:edit-part", { detail: { id: id || null } }));
  }

  binList.addEventListener("click", function (e) {
    var ed = closest(e.target, "[data-edit-part]");
    if (ed) { e.stopPropagation(); editPart(ed.getAttribute("data-edit-part")); return; }
    var cardEl = closest(e.target, "[data-template]");
    if (cardEl) placeAtCentre(cardEl.getAttribute("data-template"));
  });
  binList.addEventListener("keydown", function (e) {
    var cardEl = closest(e.target, "[data-template]");
    if (cardEl && e.target === cardEl && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      placeAtCentre(cardEl.getAttribute("data-template"));
    }
  });
  binList.addEventListener("dragstart", function (e) {
    var cardEl = closest(e.target, "[data-template]");
    if (!cardEl) return;
    try { e.dataTransfer.setData(DRAG_PART, cardEl.getAttribute("data-template")); e.dataTransfer.effectAllowed = "copy"; } catch (_) { /* no data store */ }
  });
  function draggingPart(e) {
    var types = e.dataTransfer ? Array.prototype.slice.call(e.dataTransfer.types || []) : [];
    return types.indexOf(DRAG_PART) >= 0;
  }
  wrap.addEventListener("dragover", function (e) {
    if (!model || !draggingPart(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  });
  wrap.addEventListener("drop", function (e) {
    if (!model || !draggingPart(e)) return;
    var id = e.dataTransfer.getData(DRAG_PART);
    if (!id || !hasOwn(templates, id)) return;
    e.preventDefault();
    var w = toWorld(e), t = templates[id];
    placePart(id, [w.x - (t.width || DEFAULT_WIDTH) / 2, w.y - 30], [w.x, w.y]);
  });

  // A repo template saved by the part editor: the catalog is re-read, and
  // the bench on screen, edits and all, is redrawn from it.
  document.addEventListener("embarch:parts-changed", function () {
    api(ROUTE_PARTS).then(function (res) {
      takeCatalog(res);
      renderBin();
      if (bench) {
        buildModel();
        if (sel && !(sel.kind === "part" ? model.parts[sel.id] : cableById(sel.id))) sel = null;
        render();
        renderInspector();
        renderNotes();
        scheduleCheck();
      }
    }).catch(function (e) {
      notice("the part templates could not be re-read: " + errText(e), "error");
    });
  });

  // ---------------------------------------------------------------- the dialog
  // One dialog for the editor: a new bench's name, which port function a
  // dropped port is, unsaved changes, a file changed on disk. Resolves with
  // the input or the choice, `true` for OK, and null for Cancel.
  function dialog(o) {
    if (dlgState) closeDialog(null);
    return new Promise(function (resolve) {
      dlgState = { resolve: resolve, onOk: o.onOk || null, input: !!o.input, opener: document.activeElement, busy: false };
      dlg.title.textContent = o.title || "";
      dlg.text.textContent = o.text || "";
      dlg.text.hidden = !o.text;
      dlg.field.hidden = !o.input;
      if (o.input) {
        dlg.label.textContent = o.input.label || "";
        dlg.input.value = o.input.value || "";
        dlg.input.placeholder = o.input.placeholder || "";
      }
      var choices = o.choices || [];
      dlg.choices.innerHTML = choices.map(function (c) {
        return '<button type="button" class="btn" data-choice="' + esc(c.value) + '">' + esc(c.label) + "</button>";
      }).join("");
      dlg.choices.hidden = !choices.length;
      dlg.error.hidden = true;
      dlg.error.textContent = "";
      dlg.ok.hidden = !o.ok;
      dlg.ok.textContent = o.ok || "OK";
      dlg.ok.className = "btn " + (o.danger ? "bn-danger" : "btn-primary");
      dlg.ok.disabled = false;
      dlg.cancel.textContent = o.cancel || "Cancel";
      dlg.backdrop.style.display = "block";
      dlg.box.style.display = "block";
      var first = o.input ? dlg.input : choices.length ? dlg.choices.querySelector("button") : dlg.cancel;
      first.focus();
    });
  }
  function closeDialog(value) {
    var st = dlgState;
    if (!st) return;
    dlgState = null;
    dlg.box.style.display = "none";
    dlg.backdrop.style.display = "none";
    if (st.opener && st.opener.focus && document.contains(st.opener)) {
      try { st.opener.focus({ preventScroll: true }); } catch (_) { /* gone */ }
    }
    st.resolve(value);
  }
  function dialogOk() {
    var st = dlgState;
    if (!st || st.busy) return;
    var value = st.input ? dlg.input.value : true;
    if (!st.onOk) { closeDialog(value); return; }
    st.busy = true;
    dlg.ok.disabled = true;
    Promise.resolve(st.onOk(value)).then(function (err) {
      if (dlgState !== st) return;
      st.busy = false;
      dlg.ok.disabled = false;
      if (err) { dlg.error.textContent = err; dlg.error.hidden = false; return; }
      closeDialog(value);
    });
  }
  dlg.ok.addEventListener("click", dialogOk);
  dlg.cancel.addEventListener("click", function () { closeDialog(null); });
  dlg.backdrop.addEventListener("click", function () { closeDialog(null); });
  dlg.choices.addEventListener("click", function (e) {
    var b = closest(e.target, "[data-choice]");
    if (b) closeDialog(b.getAttribute("data-choice"));
  });
  dlg.input.addEventListener("keydown", function (e) {
    if (e.key === "Enter") { e.preventDefault(); dialogOk(); }
  });

  // Unsaved edits before leaving them: ask, and on Discard put the bench back
  // the way its file says, so coming back shows the file, not the edits.
  // The part editor (bench-parts.js) keeps a draft of its own; a leave asks
  // about it too. A page without the editor has nothing unsaved there.
  function partsUnsaved() {
    var pa = window.embarchParts;
    try { return !!(pa && typeof pa.unsaved === "function" && pa.unsaved()); } catch (_) { return false; }
  }

  // Asks, and decides nothing else (fix round 1): what was discarded is
  // dropped only once the switch it was asked for happens — a bench or
  // project that loads replaces it, a create that succeeds opens the new
  // bench — so a repo path that does not exist, or a Cancel on a new bench's
  // name, keeps the edits. A tab switch cannot fail, so leaving the tab
  // discards at once. A save on its way is waited for first: a discard
  // under it would leave the old content on screen as "unsaved", and the
  // next Save would put it back over the file.
  function confirmLeave(what) {
    if (saveInFlight) {
      // A save that came back refused opened its own dialog (a 409's
      // Reload): that is what the human answers, and the leave waits for
      // another try rather than closing it unseen.
      var again = function () { return dlgState ? false : confirmLeave(what); };
      return saveInFlight.then(again, again);
    }
    var benchDirty = isDirty(), partDirty = partsUnsaved();
    if (!benchDirty && !partDirty) return Promise.resolve(true);
    var failedSave = lastSaveError;
    var where = { tab: "leave the Topology tab", project: "switch project", desk: "open another desk", bench: "open another bench" }[what] || "go on";
    var held = [];
    if (benchDirty) held.push((bench.name || bench.slug) + " has changes that are not in embarch/topologies/" + bench.slug + ".toml");
    if (partDirty) held.push("the part editor holds a template that is not saved");
    var text = held.join(", and ");
    return dialog({
      title: "Unsaved changes",
      text: (failedSave && benchDirty ? "The last save did not land: " + failedSave + ". " : "") +
        text.charAt(0).toUpperCase() + text.slice(1) + ". Discard " + (held.length > 1 ? "both" : "it") + " to " + where + "?",
      ok: "Discard changes",
      danger: true,
      cancel: "Keep editing"
    }).then(function (ok) {
      if (!ok) return false;
      leavePending = what;
      leaveParts = partDirty;
      // A tab switch cannot fail: both halves' unsaved work goes now. Any
      // other leave drops them where the switch lands (leaveLanded).
      if (what === "tab") { discardEdits(); leaveLanded(); }
      return true;
    });
  }

  // The switch a leave was confirmed for happened: the part editor's draft
  // goes too, when the leave was asked about it. The bench's own edits are
  // replaced by whatever loaded.
  function leaveLanded() {
    var pa = window.embarchParts;
    if (leavePending && leaveParts && pa && typeof pa.discard === "function") {
      try { pa.discard(); } catch (_) { /* its own dialog says what it could not do */ }
    }
    leavePending = null;
    leaveParts = false;
  }

  // The switch a leave was confirmed for did not happen: the edits stay,
  // still unsaved, and so does the part editor's draft.
  function leaveDone(ok) {
    if (ok) { leaveLanded(); return; }
    leavePending = null;
    leaveParts = false;
  }
  function discardEdits() {
    if (!bench || saved == null || !isDirty()) return;
    cancelTransient();
    restoreContent(saved);
    undoStack = [];
    redoStack = [];
    buildModel();
    placeVerdicts();   // the bench is the file again, as when it was loaded
    changed();
  }

  // ---------------------------------------------------------------- saving
  // Resolves true once the file says what is on screen. `quiet`: the caller
  // goes on (Save and make live), so the live bench is not re-applied here.
  function save(quiet) {
    if (!bench || !model || saving) return Promise.resolve(false);
    if (!isDirty()) {
      if (!quiet) notice("Nothing to save: embarch/topologies/" + bench.slug + ".toml already says this.", "info");
      return Promise.resolve(true);
    }
    saving = true;
    renderDirty();
    var sent = snapshotContent(), slug = bench.slug, mine = benchLoads;
    // The answer is about the bench it was sent for. A project switch that
    // could not ask (a desk opened elsewhere) may have replaced it by the
    // time it lands: then it is dropped whole — no version adopted, no 409
    // dialog or notice about the old repo's file over the new one, and
    // nothing for the next leave to quote.
    var gone = function () { return !bench || benchLoads !== mine; };
    lastSaveError = null;
    var landed = false;
    saveInFlight = send("PUT", ROUTE_BENCHES + "/" + encodeURIComponent(slug), benchBody()).then(function (b) {
      saving = false;
      saveInFlight = null;
      if (gone()) { renderDirty(); return; }
      bench.version = b.version;
      bench.legacy = !!b.legacy;
      // The file as it now reads is the working copy, unless the bench was
      // edited while the save was on its way.
      if (snapshotContent() === sent) restoreContent(canon(contentOf(b)));
      saved = canon(contentOf(b));
      renderPath();
      changed();
      notice("Saved embarch/topologies/" + slug + ".toml", "ok");
      document.dispatchEvent(new CustomEvent("embarch:bench-saved", { detail: { slug: slug } }));
      landed = true;
      // **Write-through** (owner): the live bench's file is what Core was
      // told, so a save of it is declared again — as a write-through, which
      // never takes the machine. While another repo holds it, or the live
      // config is gone, the save is said as saved and not declared.
      if (!quiet && isLiveBench()) writeThrough("Saved, and declared again", "Saved, not declared");
      else if (!quiet && live && !live.missing && live.slug === slug && (superseded || live.config_missing)) {
        var notWhy = superseded ? "superseded by " + repoName(superseded.repo) + " · " + superseded.slug : "the live config " + live.config + " is gone";
        wt = { ok: false, text: "saved, not declared", title: "Saved, not declared: " + notWhy };
        notice("Saved, not declared: " + notWhy, "error");
        renderLiveBar();
      }
    }).catch(function (e) {
      saving = false;
      saveInFlight = null;
      if (gone()) { renderDirty(); return; }
      lastSaveError = errText(e);
      renderDirty();
      if (e && e.status === 409) { conflict(e); return; }
      notice("Not saved: " + errText(e), "error");
    });
    return saveInFlight.then(function () { return landed; }, function () { return false; });
  }

  // A 409 on a save. The server names which it is in a header
  // (`x-embarch-refusal`), and only `version-changed` — the file changed on
  // disk since it was read — is cured by reading it again, so only that one
  // offers Reload, never an overwrite. Any other (a file that no longer
  // parses, a rename, no project open) is said in the server's words.
  function conflict(e) {
    var slug = bench.slug;
    if (e.code !== "version-changed") { notice("Not saved: " + errText(e), "error"); return; }
    dialog({
      title: "The file changed on disk",
      text: errText(e) + ". Reload reads it again and drops the unsaved edits on screen; Keep editing leaves the file as it is.",
      ok: "Reload from disk",
      danger: true,
      cancel: "Keep editing"
    }).then(function (ok) {
      if (ok && bench && bench.slug === slug) loadBench(slug, false);
    });
  }

  // "New bench…": a name, then a file holding this machine — the served
  // template that is a hub — and one config, A.
  function newBench() {
    var all = Object.keys(templates).map(function (id) { return templates[id]; });
    var hub = all.filter(function (t) { return t.hub && t.source !== "repo"; })[0] || all.filter(function (t) { return t.hub; })[0];
    if (!hub) { notice("no part template is a hub (this machine), so a bench cannot start from one", "error"); return; }
    var made = null;
    dialog({
      title: "New bench",
      text: "Written as embarch/topologies/<name>.toml, holding this machine and one config, " + AUTO_CONFIG + ".",
      input: { label: "Name", value: "", placeholder: "pd-bench" },
      ok: "Create",
      cancel: "Cancel",
      onOk: function (value) {
        var body = { name: String(value || "").trim(), parts: [newInst(hub.id, String(hub.id), HUB_HOME)], cables: [],
                     configs: [{ name: AUTO_CONFIG, cables: [] }], signals: [] };
        return send("POST", ROUTE_BENCHES, body).then(function (b) { made = b; return null; }, errText);
      }
    }).then(function (ok) {
      if (!ok || !made) { leaveDone(false); return; }
      remember(made.slug);
      document.dispatchEvent(new CustomEvent("embarch:bench-saved", { detail: { slug: made.slug } }));
      loadAll(false, made.slug);
    });
  }

  // ---------------------------------------------------------------- making a bench live
  // Phase 4 (embarch-bench-editor-proposal.md, "Making a bench live"): which
  // bench and config are live is this UI's record, never the repo's, and
  // **one bench is live per machine** (owner, 2026-11-01): the last explicit
  // Make live wins, and a repo that held the machine reads `superseded`.
  // **Make live** applies what the bench *as saved* declares to Core and
  // answers a report: what applied, the enrolments it proposes (each
  // confirmed here, reading the probe's hardware ID), what it could not
  // propose, and what Core holds that the bench does not name, each with its
  // own Retract. **Nothing is retracted without a click**, and **nothing takes
  // the machine without one**: a save of the live bench, a confirmed
  // proposal and Declare again re-apply with `write_through: true`, which the
  // server refuses unless this repo's live bench holds the machine — and a
  // refused write-through is said as "saved, not declared", never as a
  // reason to take the machine. Switching config tabs never calls Core.
  function isLiveBench() {
    return !!live && !live.missing && !live.unparseable && !live.config_missing && !superseded && !!bench && live.slug === bench.slug;
  }

  // Which repo a path names, as the sidebar names it.
  function repoName(path) {
    var parts = String(path || "").split(/[\\/]/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : String(path || "another repo");
  }

  // A read has its own sequence, so it never cancels a write on its way;
  // a write begun after it was asked answers with something newer, so the
  // read gives way to it (and is asked again if that write does not land).
  function readLive() {
    var seq = ++readSeq, gen = projectGen, writes = liveSeq;
    var stale = function () {
      if (seq !== readSeq || gen !== projectGen) return true;
      if (writes !== liveSeq) { readDropped = true; return true; }
      return false;
    };
    return api(ROUTE_LIVE).then(function (r) {
      if (stale()) return;
      live = (r && r.live) || null;
      superseded = (r && r.superseded) || null;
      liveState = "known";
      liveError = null;
      renderLiveBar();
      renderConfigs();
      renderLiveReport();
    }).catch(function (e) {
      if (stale()) return;
      live = null;
      superseded = null;
      // A server without the routes: no live controls, no claim either way.
      // Anything else — a bench file that cannot be read is a 500 — is
      // "unknown", said with its reason: never a state of the file.
      liveState = e && e.status === 404 ? "unavailable" : "error";
      liveError = errText(e);
      renderLiveBar();
      renderConfigs();
      renderLiveReport();
    });
  }

  function sinceText(ms) {
    if (!ms) return "";
    try { return new Date(ms).toLocaleString(); } catch (_) { return String(ms); }
  }

  function setBtn(el, show, text, title) {
    el.hidden = !show;
    if (show && text != null) el.textContent = text;
    if (show && title != null) el.title = title;
  }

  function renderLiveBar() {
    var routes = liveState !== "unavailable" && liveState !== "unknown";
    var on = !!bench && !!model;
    [makeLiveBtn, stopLiveBtn, declareBtn, retryBtn, wtBtn].forEach(function (b) { b.hidden = true; });
    liveBtn.hidden = !liveReport || !routes;
    liveEl2.hidden = !routes || !on;
    if (!routes || !on) return;
    var cls = "bench-live", text, title = "", here = !!live && !!bench && live.slug === bench.slug;
    if (liveState === "error") {
      cls += " bench-live-unknown";
      text = "unknown: " + liveError;
      title = "Which bench is live could not be read: " + liveError;
      setBtn(retryBtn, true);
    } else if (!live) {
      cls += " bench-live-none";
      text = "not live";
      title = "No bench is live for this repo";
    } else if (live.missing || live.unparseable) {
      cls += " bench-live-missing";
      text = "live: " + live.slug + (live.missing ? " — its file is gone" : " — its file does not parse");
      title = live.missing ? "The record names embarch/topologies/" + live.slug + ".toml, which is not there any more"
        : "embarch/topologies/" + live.slug + ".toml does not parse, so what is live cannot be read from it";
    } else if (superseded) {
      cls += " bench-live-superseded";
      text = "superseded by " + repoName(superseded.repo) + " · " + superseded.slug;
      title = "One bench is live per machine: " + superseded.slug + " in " + superseded.repo + " went live after " + live.slug +
        " did here. This repo's saves do not declare anything until Make live takes the machine back.";
    } else if (here && live.config_missing) {
      cls += " bench-live-missing";
      text = "live config " + live.config + " is gone";
      title = "The record names config " + live.config + ", which " + live.slug + " no longer has: make a config it has live";
    } else if (here) {
      cls += " bench-live-here";
      text = "live" + (live.config ? " · " + live.config : "");
      title = "This bench is live on this machine" + (live.config ? ", wired as config " + live.config : ", its bench-level cables alone") +
        (live.since_utc_ms ? ", since " + sinceText(live.since_utc_ms) : "");
    } else {
      cls += " bench-live-other";
      text = "live: " + live.slug;
      title = "Another bench in this repo is the live one";
    }
    liveEl2.className = cls;
    liveEl2.textContent = text;
    liveEl2.title = title;
    if (liveState === "error") return;
    var dirty = isDirty(), cfg = cfgName(), live_here = isLiveBench();
    if (!live_here || cfg !== live.config) {
      var verb = dirty ? "Save and make " : "Make ";
      setBtn(makeLiveBtn, true, verb + (live_here && cfg != null ? cfg + " " : "") + "live",
        superseded && here ? "Take the machine back: declare this bench and record it live here" + (dirty ? " — saved first" : "")
          : live_here ? "Declare this bench again, wired as config " + cfg + ", and record that config as the live wiring"
          : "Declare what this bench says to Core and record it live on this machine" + (dirty ? " — saved first" : ""));
    }
    makeLiveBtn.disabled = liveBusy || saving;
    if (here || live && (live.missing || live.unparseable)) {
      setBtn(stopLiveBtn, true);
      stopLiveBtn.disabled = liveBusy;
    }
    if (live_here) {
      setBtn(declareBtn, true, "Declare again", "Apply this bench again as saved — a failed row, or the dev-bench link once its role holds its board, without an edit");
      declareBtn.disabled = liveBusy || saving;
    }
    if (wt && here) {
      setBtn(wtBtn, true, wt.text, wt.ok ? "What the last automatic declare said: open its report" : wt.title || wt.text);
      wtBtn.className = "btn bench-live-wt" + (wt.ok ? "" : " bench-live-wt-failed");
    }
  }

  // Saved first when edits are on screen, then declared with the version the
  // save answered; the config is the tab on screen. An explicit make-live
  // takes the machine.
  function makeLive() {
    if (!bench || !model || liveBusy) return;
    var cfg = cfgName();
    save(true).then(function (ok) { if (ok) goLive({ slug: bench.slug, config: cfg, explicit: true }); });
  }

  // A re-apply nobody clicked Make live for: after a Save of the live bench,
  // after a Confirm, and Declare again. `fail` is how its caller says a
  // refusal ("Saved, not declared", "Not declared again"); `confirmed` the
  // enrolled marks a Confirm carries into the report its own re-apply
  // answers, and into no later one.
  function writeThrough(said, fail, confirmed) {
    if (!live || live.missing || live.unparseable) return;
    goLive({ slug: live.slug, config: live.config == null ? null : live.config, explicit: false, said: said,
      fail: fail || "Not declared again", confirmed: confirmed || null });
  }

  // The version to send: the bench on screen's, once any save on its way has
  // landed, else the file's as the server reads it now.
  function versionFor(slug) {
    var wait = saveInFlight ? saveInFlight.then(function () {}, function () {}) : Promise.resolve();
    return wait.then(function () {
      if (bench && bench.slug === slug) return bench.version;
      return api(ROUTE_BENCHES + "/" + encodeURIComponent(slug)).then(function (b) { return b && b.version; });
    });
  }

  // A row that changed a role's board type or the dev-bench link changes
  // what the last Validate checked. A re-apply re-sends every row, so only a
  // row the last report did not already show applied, word for word, counts
  // — every row on an explicit make-live.
  function rowSig(a) { return [a.route || "", a.what || "", a.text || ""].join("\u0000"); }
  function rolesTouched(applied, before) {
    var out = [], had = dict();
    ((before && before.applied) || []).forEach(function (a) { if (a && a.ok) had[rowSig(a)] = true; });
    (applied || []).forEach(function (a) {
      if (!a || !a.ok || had[rowSig(a)]) return;
      var role = a.role || null, m = /\/probes\/enrolled\/([^/\s]+)\/board/.exec(String(a.route || ""));
      if (!role && m) role = decodeURIComponent(m[1]);
      if (!role && /\/dev-bench\/link/.test(String(a.route || ""))) role = "dev-bench";
      if (role && out.indexOf(role) < 0) out.push(role);
    });
    return out;
  }

  // A write — make-live, write-through or Stop — is counted while on its
  // way; its answer settles the count whether it is used or dropped, so a
  // dropped answer never leaves the buttons waiting. True when the answer
  // is still the newest write's in this project.
  function writeBegun() {
    liveWrites++;
    liveBusy = true;
    return { gen: projectGen, seq: ++liveSeq };
  }
  function writeSettled(w) {
    if (w.gen !== projectGen) return false;   // another repo since: its own count started at 0
    liveWrites = Math.max(0, liveWrites - 1);
    liveBusy = liveWrites > 0;
    if (w.seq !== liveSeq) { renderLiveBar(); return false; }
    return true;
  }

  // The report an answer brings, kept whole: its marks only those passed in.
  function takeReport(r, o) {
    liveReport = r;
    liveReport.id = ++reportSeq;
    liveReport.slug = o.slug;
    liveReport.said = o.said || null;
    liveReport.confirmed = o.confirmed || dict();
    liveRows = dict();
  }

  function goLive(o) {
    var w = writeBegun(), mine = benchLoads;
    renderLiveBar();
    if (o.explicit) notice("Declaring " + o.slug + " to Core…", "info");
    versionFor(o.slug).then(function (version) {
      if (w.gen !== projectGen) return null;
      // Required either way: false is the button's take of the machine,
      // true a re-apply that never takes it.
      var body = { config: o.config, version: version == null ? null : version, write_through: !o.explicit };
      return send("POST", ROUTE_BENCHES + "/" + encodeURIComponent(o.slug) + "/live", body);
    }).then(function (r) {
      if (!writeSettled(w)) return;   // another repo, or a newer write, since
      readDropped = false;            // this answer is newer than any read it displaced
      var before = o.explicit ? null : liveReport;
      if (!r) { liveState = "unknown"; renderLiveBar(); readLive(); return; }
      var props = (r.proposals || []).length, failed = (r.applied || []).filter(function (a) { return a && !a.ok; }).length;
      if (!r.live) {
        // Core was told, and the record of which bench is live was not
        // written (an applied row says why): the report is kept — its
        // buttons wait, since it is no live bench's — and the record is
        // read again for what the server holds.
        var row = (r.applied || []).filter(function (a) { return a && !a.ok && a.what === "live record"; })[0];
        var why = "Declared, not recorded live: " + (row ? row.text : "the answer named no live record");
        takeReport(r, o);
        if (!o.explicit) wt = { ok: false, text: "declared, not recorded", title: why };
        if (benchLoads === mine || !o.explicit) notice(why, "error");
        renderLiveBar();
        renderLiveReport();
        if (o.explicit) openLivePanel();
        dropVerdicts(r.applied, before);
        readLive();
        return;
      }
      live = r.live;
      superseded = null;
      liveState = "known";
      takeReport(r, o);
      if (o.explicit) {
        wt = null;
        if (benchLoads === mine) notice(o.slug + " is live on this machine", "ok");
      } else {
        wt = { ok: true, text: "declared" + (props ? " · " + props + " proposal" + (props === 1 ? "" : "s") : "") +
          (failed ? " · " + failed + " failed" : "") };
      }
      renderLiveBar();
      renderConfigs();
      renderLiveReport();
      if (o.explicit) openLivePanel();
      dropVerdicts(r.applied, before);
      document.dispatchEvent(new CustomEvent("embarch:live-changed", { detail: { slug: live.slug, config: live.config == null ? null : live.config } }));
    }, function (e) {
      if (!writeSettled(w)) return;
      var again = readDropped;
      readDropped = false;
      if (!o.explicit) {
        // A write-through that did not land: Core was not told, and that
        // stays said — in the words of whoever asked — until a declare lands.
        wt = { ok: false, text: o.fail.toLowerCase(), title: o.fail + ": " + errText(e) };
        notice(o.fail + ": " + errText(e), "error");
        renderLiveBar();
        renderLiveReport();
        readLive();
        return;
      }
      renderLiveBar();
      if (again || (e && (e.status === 409 || e.status >= 500))) readLive();
      if (benchLoads !== mine) return;
      if (e && e.status === 409 && e.code === "version-changed") { conflict(e); return; }
      notice("Not made live: " + errText(e), "error");
    });
  }

  // Validate's verdicts on the roles an answer's rows changed go.
  function dropVerdicts(applied, before) {
    rolesTouched(applied, before).forEach(function (role) {
      document.dispatchEvent(new CustomEvent("embarch:role-changed", { detail: { role: role, probe_serial: null } }));
    });
  }

  // The record goes; Core is not touched.
  function stopLive() {
    if (liveBusy) return;
    var w = writeBegun();
    renderLiveBar();
    fetch(ROUTE_LIVE, { method: "DELETE" }).then(function (r) {
      if (!r.ok && r.status !== 204) return r.text().then(function (t) { throw new Error(t || "HTTP " + r.status); });
    }).then(function () {
      if (!writeSettled(w)) return;
      readDropped = false;
      live = null;
      superseded = null;
      wt = null;
      notice("No bench is live for this repo now; Core holds what it held", "info");
      renderLiveBar();
      renderConfigs();
      renderLiveReport();
      document.dispatchEvent(new CustomEvent("embarch:live-changed", { detail: { slug: null, config: null } }));
    }, function (e) {
      if (!writeSettled(w)) return;
      renderLiveBar();
      notice("Still live: " + errText(e), "error");
      if (readDropped) { readDropped = false; readLive(); }
    });
  }

  // Opened by the page, not by the human's click on its button: focus lands
  // on the panel itself, never on a button inside it — one Space must not
  // confirm an enrolment nobody chose.
  // `opener`: the bar's indicator, which Esc gives focus back to.
  function openLivePanel(opener) {
    var panel = document.getElementById("topo-panel-live");
    if (!liveBtn.classList.contains("is-open")) {
      openingLive = true;
      try { liveBtn.click(); } finally { openingLive = false; }
    }
    liveOpener = opener || null;
    if (panel && !panel.hidden) panel.focus({ preventScroll: true });
  }
  // The Live report button itself is its own opener (app.js returns focus
  // to it); a report the indicator opened gives focus back to the
  // indicator, once app.js has closed the panel on Esc — heard on capture,
  // so before app.js's own listener closes it.
  liveBtn.addEventListener("click", function () { if (!openingLive) liveOpener = null; });
  document.addEventListener("keydown", function (e) {
    if (e.key !== "Escape" || !liveOpener) return;
    var panel = document.getElementById("topo-panel-live"), back = liveOpener;
    if (!panel || panel.hidden) return;
    setTimeout(function () {
      if (!panel.hidden) return;
      liveOpener = null;
      if (!back.hidden && !back.disabled) back.focus({ preventScroll: true });
    }, 0);
  }, true);

  function liveRow(badgeHtml, cls, head, text, extra) {
    return '<div class="topo-check bench-live-row' + (cls ? " " + cls : "") + '"><div class="topo-check-head">' + badgeHtml +
      '<span class="topo-check-label">' + head + "</span>" + (extra || "") + "</div>" +
      (text ? '<p class="placeholder-note" style="margin:4px 0 0;">' + esc(text) + "</p>" : "") + "</div>";
  }

  function badge(kind, text) { return '<span class="badge badge-' + kind + '">' + esc(text) + "</span>"; }
  function propKey(pr) { return "p|" + (pr.role || "") + "|" + (pr.probe_serial || "") + "|" + (pr.part || ""); }
  function leftKey(lo) { return "l|" + lo.kind + "|" + lo.id; }

  // Whether the report on screen is still the live bench's: its buttons act
  // only while it is.
  function reportCurrent() {
    var l = liveReport && liveReport.live;
    return !!l && !!live && !live.missing && !live.unparseable && !live.config_missing && !superseded && live.slug === l.slug &&
      (live.config == null ? null : live.config) === (l.config == null ? null : l.config);
  }

  function renderLiveReport() {
    var r = liveReport;
    if (!r) {
      liveHead.textContent = "Live";
      liveBody.innerHTML = '<p class="placeholder-note">No bench has been made live here yet.</p>';
      liveBtn.hidden = true;
      return;
    }
    var l = r.live || {}, still = reportCurrent();
    liveHead.textContent = still ? "Live: " + l.slug + (l.config ? " · config " + l.config : "")
      : r.live ? "Last made live: " + l.slug : "Declared: " + r.slug;
    var why = still ? "since " + sinceText(l.since_utc_ms)
      : !r.live ? "declared to Core, but not recorded live — its buttons wait until it is"
      : superseded ? "superseded by " + repoName(superseded.repo) + " · " + superseded.slug + " — its buttons act no more"
      : live && live.slug === l.slug && (live.config || null) !== (l.config || null) ? "config " + (live.config || "none") + " is live now — its buttons act no more"
      : "no longer the live bench — its buttons act no more";
    var h = '<div class="topo-check-summary">' + (r.said ? '<span class="badge badge-neutral">' + esc(r.said) + "</span> " : "") +
      '<span class="placeholder-note">' + esc(why) + "</span></div>";
    if (wt && !wt.ok) h += '<p class="sd-error" style="margin:6px 0 0;">' + esc(wt.title || wt.text) + "</p>";
    var applied = r.applied || [], props = r.proposals || [], notp = r.not_proposed || [], left = r.leftovers || [];
    var off = still ? "" : " disabled";
    h += '<div class="card-title bench-live-sec">Applied · ' + applied.length + "</div>";
    h += applied.length ? applied.map(function (a) {
      return liveRow(a.ok ? badge("success", "applied") : badge("danger", a.route === "(not sent)" ? "not sent" : "failed"),
        a.ok ? "" : "bench-live-failed",
        esc(a.what) + (a.route ? ' <span class="mono bench-live-route">' + esc(a.route) + "</span>" : ""), a.text);
    }).join("") : '<p class="placeholder-note">This bench declares nothing to apply.</p>';
    h += '<div class="card-title bench-live-sec">Proposed · ' + props.length + "</div>";
    if (props.length) {
      h += '<p class="placeholder-note bench-live-why">Each enrolment is yours to confirm: Confirm binds the probe to ' +
        "the board and reads its hardware ID through it, the ordinary enrol.</p>";
    }
    h += props.length ? props.map(function (pr, i) {
      var key = propKey(pr), done = liveRows[key], ok = r.confirmed && r.confirmed[key];
      var who = pr.role ? pr.role : "bench board";
      var btn = ok ? badge("success", "enrolled")
        : '<button type="button" class="btn btn-primary" data-live-confirm="' + i + '"' + (done && done.busy ? " disabled" : off) + ">Confirm</button>";
      var text = (pr.text || "") + " · chip " + (pr.chip || "?") + " · probe " + (pr.probe_serial || "?") +
        (ok ? " — " + ok : done && done.text ? " — " + done.text : "");
      return liveRow(badge("neutral", who), done && done.ok === false ? "bench-live-failed" : "",
        esc(pr.name || pr.part || ""), text, '<span style="flex:1;"></span>' + btn);
    }).join("") : '<p class="placeholder-note">Nothing to enrol.</p>';
    if (notp.length) {
      h += '<div class="card-title bench-live-sec">Not proposed · ' + notp.length + "</div>";
      h += notp.map(function (n) { return liveRow(badge("warning", "not proposed"), "", esc(n.part || ""), n.text); }).join("");
    }
    h += '<div class="card-title bench-live-sec">In Core, not on this bench · ' + left.length + "</div>";
    if (left.length) {
      h += '<p class="placeholder-note bench-live-why">Core&rsquo;s store is this machine&rsquo;s, not this repo&rsquo;s: ' +
        "nothing here is retracted unless you press its Retract.</p>";
    }
    h += left.length ? left.map(function (lo, i) {
      var done = liveRows[leftKey(lo)];
      return liveRow(badge("neutral", lo.kind), done && done.ok === false ? "bench-live-failed" : "", '<span class="mono">' + esc(lo.id) + "</span>",
        (lo.text || "") + (done && done.text ? " — " + done.text : ""),
        '<span style="flex:1;"></span><button type="button" class="btn btn-icon-danger" data-live-retract="' + i + '"' +
        (done && done.busy ? " disabled" : off) + ">Retract</button>");
    }).join("") : '<p class="placeholder-note">Nothing: Core holds only what this bench names.</p>';
    liveBody.innerHTML = h;
    liveBtn.hidden = false;
  }

  // The ordinary enrol, with the proposal's fields as the server wrote them —
  // for a role its `name` is the board type, never the part's label
  // (decision 45). Core's answer is said, the role it moved included
  // (decision 61). Then the live bench is declared again, as a write-through:
  // what waited on that role holding its board (the dev-bench link) lands
  // without a second click.
  function confirmProposal(i) {
    var r = liveReport, pr = r && (r.proposals || [])[i];
    if (!pr || !reportCurrent()) return;
    var key = propKey(pr), id = r.id, gen = projectGen;
    liveRows[key] = { busy: true, text: "enrolling — reading the hardware ID through probe " + (pr.probe_serial || "?") + "…" };
    renderLiveReport();
    send("POST", ROUTE_ENROLL, { role: pr.role || "", chip: pr.chip || "", probe_serial: pr.probe_serial || null, name: pr.name || null }).then(function (b) {
      if (gen !== projectGen) return;
      var who = pr.role ? pr.role : pr.name || pr.part || "the bench board";
      var said = "Enrolled " + who + (b && b.chip ? " as " + b.chip : "") + (b && b.moved_roles_note ? " — " + b.moved_roles_note : "");
      // A new probe binding is a new thing to check (decision 45).
      document.dispatchEvent(new CustomEvent("embarch:role-changed", { detail: { role: pr.role || null, probe_serial: pr.probe_serial || null } }));
      // The mark goes on the report it was pressed in, if that is still on
      // screen, and rides into the one its own re-apply answers — no further.
      if (liveReport && liveReport.id === id) {
        liveReport.confirmed[key] = said;
        delete liveRows[key];
        renderLiveReport();
      }
      var carry = dict();
      carry[key] = said;
      // Always declared again (the server refuses when it should): a save's
      // re-apply that replaced the report meanwhile ran before this enrol.
      writeThrough(said + "; declared again", "Not declared again", carry);
    }).catch(function (e) {
      if (!liveReport || liveReport.id !== id) return;
      liveRows[key] = { ok: false, text: "not enrolled: " + errText(e) };
      renderLiveReport();
    });
  }

  // One leftover, by its own click; the list comes back from the answer.
  function retractLeftover(i) {
    var r = liveReport, lo = r && (r.leftovers || [])[i];
    if (!lo || !reportCurrent()) return;
    var key = leftKey(lo), id = r.id;
    liveRows[key] = { busy: true, text: "retracting…" };
    renderLiveReport();
    send("POST", ROUTE_RETRACT, { kind: lo.kind, id: lo.id }).then(function (a) {
      if (!liveReport || liveReport.id !== id) return;
      if (a && Array.isArray(a.leftovers)) liveReport.leftovers = a.leftovers;
      if (a && a.ok) delete liveRows[key];
      else liveRows[key] = { ok: false, text: "not retracted: " + ((a && a.text) || "Core refused") };
      notice(a && a.ok ? "Retracted " + lo.kind + " " + lo.id + (a.text && a.text !== "retracted" ? ": " + a.text : "") :
        "Not retracted: " + ((a && a.text) || "Core refused"), a && a.ok ? "ok" : "error");
      renderLiveReport();
      if (a && a.ok && lo.kind === "role") {
        document.dispatchEvent(new CustomEvent("embarch:role-changed", { detail: { role: lo.id, probe_serial: null } }));
      }
    }).catch(function (e) {
      if (!liveReport || liveReport.id !== id) return;
      liveRows[key] = { ok: false, text: "not retracted: " + errText(e) };
      renderLiveReport();
      if (e && (e.status === 409 || e.status >= 500)) readLive();
    });
  }

  liveBody.addEventListener("click", function (e) {
    var c = closest(e.target, "[data-live-confirm]");
    if (c && !c.disabled) { confirmProposal(+c.getAttribute("data-live-confirm")); return; }
    var r = closest(e.target, "[data-live-retract]");
    if (r && !r.disabled) retractLeftover(+r.getAttribute("data-live-retract"));
  });

  // The Saved benches panel's Open and the desk banner's review (app.js):
  // that bench on the canvas, Make live offered for the human to press.
  // A list still on its way (the desk's project switch) is waited for
  // before "this repo does not have" is said.
  function showBench(slug, why) {
    if (!slug) return;
    var gen = projectGen;
    whenListed(function () {
      if (gen !== projectGen) return;
      var listed = benchList.some(function (b) { return b.slug === slug; });
      if (!listed) { notice((why || "That names") + " embarch/topologies/" + slug + ".toml, which this repo does not have", "error"); return; }
      var offer = function () { notice((why ? why + ": " : "") + slug + " is on the canvas — Make live declares it", "info"); };
      if (bench && bench.slug === slug) { offer(); return; }
      var go = function () { picker.value = slug; lostEdits = null; pendingOffer = { slug: slug, say: offer }; loadBench(slug); };
      if (isDirty() || partsUnsaved()) {
        confirmLeave("bench").then(function (ok) { if (ok) go(); });
        return;
      }
      go();
    });
  }

  // ---------------------------------------------------------------- a link to a board
  // `#topology?board=<…>` (decision 58) names a role or a board type; on the
  // canvas it marks the parts of the bench on screen that hold that role or
  // that board type — the slot by its wire spelling or as shown, then the
  // board exactly, then the board's own name before a `/` or `@`. app.js
  // marks the catalog rows and says when nothing anywhere matched.
  function linkKey(v) { return String(v == null ? "" : v).trim().toLowerCase(); }
  function linkBase(v) { return linkKey(v).split(/[/@]/)[0]; }
  // A role's board type as the served pickers label it (the dev bench's
  // supported pair carries a label), when they have been read.
  function boardLabel(role, board) {
    var list = pickers && role ? pickers[role] || [] : [];
    var found = list.filter(function (o) { return o && o.board === board; })[0];
    return found && found.label ? found.label : null;
  }
  function linkMatches(board) {
    var target = board === undefined ? linkBoard : board;
    if (target == null || !model) return [];
    var want = linkKey(target), base = linkBase(target);
    var tier = function (same, bySlot) {
      return model.order.filter(function (p) {
        if (p.dup) return false;
        var role = p.inst.role ? String(p.inst.role) : "";
        if (bySlot && role && (want === role || want === role.replace("-", " ") || want === role.replace("-", "_"))) return true;
        if (!p.inst.board) return false;
        var label = boardLabel(role, p.inst.board);
        return same(p.inst.board) || (!!label && same(label));
      }).map(function (p) { return p.key; });
    };
    var exact = tier(function (v) { return linkKey(v) === want; }, true);
    if (exact.length || !base) return exact;
    return tier(function (v) { return linkBase(v) === base; }, false);
  }
  function showLinked() {
    if (!pickers && !pickersAsked) loadPickers();
    var keys = linkMatches();
    render();
    if (keys.length && !linkPanned) {
      linkPanned = true;
      var p = model.parts[keys[0]];
      if (p) {
        var g = geom(p), r = wrapSize();
        view.x = g.x + g.w / 2 - r.w / view.k / 2;
        view.y = g.y + g.h / 2 - r.h / view.k / 2;
        setViewBox();
      }
    }
    document.dispatchEvent(new CustomEvent("embarch:bench-marked", { detail: { count: keys.length } }));
  }
  document.addEventListener("embarch:route", function (ev) {
    var d = (ev && ev.detail) || {};
    linkBoard = d.tab === "topology" && d.params && d.params.board ? String(d.params.board) : null;
    linkPanned = false;
    // A board's served label (the dev bench's) is in the pickers: read them,
    // and mark again once they answer.
    if (linkBoard != null && !pickers && !pickersAsked) loadPickers();
    if (model) showLinked();
  });
  document.addEventListener("embarch:route-done", function () {
    if (linkBoard == null) return;
    linkBoard = null;
    if (model) render();
  });

  // ---------------------------------------------------------------- view
  function wrapSize() {
    var r = wrap.getBoundingClientRect();
    return { w: r.width, h: r.height, left: r.left, top: r.top };
  }

  function toWorld(e) {
    var r = wrapSize();
    return { x: view.x + (e.clientX - r.left) / view.k, y: view.y + (e.clientY - r.top) / view.k };
  }

  function setViewBox() {
    var r = wrapSize();
    var W = Math.max(r.w, 10) / view.k, H = Math.max(r.h, 10) / view.k;
    svg.setAttribute("viewBox", f(view.x) + " " + f(view.y) + " " + f(W) + " " + f(H));
    var grid = document.getElementById("bench-grid");
    if (grid) {
      grid.setAttribute("x", f(view.x - 24));
      grid.setAttribute("y", f(view.y - 24));
      grid.setAttribute("width", f(W + 48));
      grid.setAttribute("height", f(H + 48));
    }
  }

  function zoomAt(factor, cx, cy) {
    var r = wrapSize();
    var px = cx == null ? r.w / 2 : cx - r.left, py = cy == null ? r.h / 2 : cy - r.top;
    var wx = view.x + px / view.k, wy = view.y + py / view.k;
    view.k = clamp(view.k * factor, ZOOM_MIN, ZOOM_MAX);
    view.x = wx - px / view.k;
    view.y = wy - py / view.k;
    setViewBox();
  }

  // Fit the whole bench, however large, centred in the canvas.
  function fit() {
    if (!model || !model.order.length) return;
    var r = wrapSize();
    if (r.w < 20 || r.h < 20) return;   // a hidden tab has no size yet; the resize observer fits it later
    cache = cache || { vis: dict(), geo: dict(), below: dict() };
    var x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    model.order.forEach(function (p) {
      var g = geom(p);
      x0 = Math.min(x0, g.x); y0 = Math.min(y0, g.y - 20);
      x1 = Math.max(x1, g.x + g.w); y1 = Math.max(y1, g.y + g.h + 26);
    });
    var bw = x1 - x0 + FIT_PAD * 2, bh = y1 - y0 + FIT_PAD * 2;
    view.k = clamp(Math.min(r.w / bw, r.h / bh), ZOOM_MIN, FIT_MAX);
    view.x = (x0 + x1) / 2 - r.w / view.k / 2;
    view.y = (y0 + y1) / 2 - r.h / view.k / 2;
    fitted = true;
    setViewBox();
  }

  // ---------------------------------------------------------------- gestures
  function select(next) {
    sel = next;
    render();
    renderInspector();
  }

  function capture(e) {
    try { svg.setPointerCapture(e.pointerId); } catch (_) { /* a synthetic pointer has none */ }
  }
  function release(e) {
    try { svg.releasePointerCapture(e.pointerId); } catch (_) { /* already released */ }
  }

  svg.addEventListener("pointerdown", function (e) {
    if (e.button !== 0 || !model) return;
    // A label half typed in the inspector lands before the canvas moves on.
    if (document.activeElement && insEl.contains(document.activeElement) && document.activeElement.blur) document.activeElement.blur();
    if (!model) return;
    var port = closest(e.target, "[data-conn]");
    var part = closest(e.target, "[data-part]");
    var hit = closest(e.target, "[data-cable-hit]");
    if (port) {
      var p = model.parts[port.getAttribute("data-pk")];
      var c = p ? p.byId[port.getAttribute("data-cid")] : null;
      var end = port.getAttribute("data-end");
      if (laying && laying.mode === "click") { e.preventDefault(); finishAt(end); return; }
      var item = trayItem();
      if (item && p && c && accepts(p, c, item)) { e.preventDefault(); bindTray(item, p.key, c.id); return; }
      if (port.getAttribute("data-cable-of")) {
        select({ kind: "cable", id: port.getAttribute("data-cable-of") });
        return;
      }
      // A free connector starts a cable; a wired one only selects its cable.
      if (p && c && end && !p.tpl.hub && !c.slot) {
        e.preventDefault();
        startLaying(p, c, e);
        capture(e);
        return;
      }
    }
    if (laying) { laying = null; render(); }
    if (trayArm) { trayArm = null; renderTray(); render(); }
    if (part) {
      var key = part.getAttribute("data-part");
      select({ kind: "part", id: key });
      if (model.parts[key]) { startDrag(model.parts[key], e); capture(e); }
      return;
    }
    if (hit) { select({ kind: "cable", id: hit.getAttribute("data-cable-hit") }); return; }
    pan = { sx: e.clientX, sy: e.clientY, vx: view.x, vy: view.y, moved: false, id: e.pointerId };
    capture(e);
  });

  svg.addEventListener("pointermove", function (e) {
    if (laying) {
      var w = toWorld(e);
      if (laying.mode === "press" && Math.abs(e.clientX - laying.sx) + Math.abs(e.clientY - laying.sy) > MOVE_SLOP) laying.mode = "drag";
      laying.x = w.x;
      laying.y = w.y;
      if (laying.mode === "press") return;
      var port = endUnder(e);
      laying.hoverConn = port ? port.getAttribute("data-conn") : null;
      laying.hoverEnd = port ? port.getAttribute("data-end") : null;
      refreshHover();
      render();
      return;
    }
    if (drag) { moveDrag(e); return; }
    if (pan) {
      var dx = e.clientX - pan.sx, dy = e.clientY - pan.sy;
      if (!pan.moved && Math.abs(dx) + Math.abs(dy) < MOVE_SLOP) return;
      pan.moved = true;
      view.x = pan.vx - dx / view.k;
      view.y = pan.vy - dy / view.k;
      setViewBox();
      return;
    }
    var pe = closest(e.target, "[data-part]");
    var id = pe ? pe.getAttribute("data-part") : null;
    if (id !== hoverPart) { hoverPart = id; render(); }
  });

  function endPan(e) {
    if (!pan) return;
    var clicked = !pan.moved;
    release(e);
    pan = null;
    if (clicked && e.type === "pointerup" && sel) select(null);
  }
  svg.addEventListener("pointerup", function (e) {
    if (laying) {
      release(e);
      if (laying.mode === "press") { laying.mode = "click"; render(); return; }
      if (laying.mode === "drag") {
        var port = endUnder(e);
        finishAt(port ? port.getAttribute("data-end") : null);
      }
      return;
    }
    if (drag) { release(e); endDrag(); return; }
    endPan(e);
  });
  svg.addEventListener("pointercancel", function (e) {
    if (laying && laying.mode !== "click") cancelLaying();
    if (drag) { release(e); endDrag(); return; }
    endPan(e);
  });
  svg.addEventListener("pointerleave", function () {
    if (!pan && !drag && !laying && hoverPart) { hoverPart = null; render(); }
  });

  // A tray item dragged over the canvas: decision 43's three properties. The
  // listeners are the svg's; the connector under the pointer is state, drawn
  // again on every rebuild; and the drop is allowed (`preventDefault`) only
  // over a connector that takes the item, never over the space between.
  svg.addEventListener("dragover", function (e) {
    if (!trayDrag || !model) return;
    var port = closest(e.target, "[data-conn]");
    var ok = !!port && port.classList.contains("bn-tray-ok");
    var key = ok ? port.getAttribute("data-conn") : null;
    var pe = closest(e.target, "[data-part]");
    var hp = pe ? pe.getAttribute("data-part") : null;
    if (key !== trayHover || hp !== hoverPart) { trayHover = key; hoverPart = hp; render(); }
    if (ok) {
      e.preventDefault();
      e.dataTransfer.dropEffect = "link";
    }
  });
  svg.addEventListener("drop", function (e) {
    if (!trayDrag || !model) return;
    var port = closest(e.target, "[data-conn]");
    var item = trayItem();
    if (!port || !item || !port.classList.contains("bn-tray-ok")) return;
    e.preventDefault();
    bindTray(item, port.getAttribute("data-pk"), port.getAttribute("data-cid"));
  });

  svg.addEventListener("wheel", function (e) {
    if (!model) return;
    e.preventDefault();
    var unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
    zoomAt(Math.exp(-e.deltaY * unit * 0.0015), e.clientX, e.clientY);
  }, { passive: false });

  document.getElementById("bench-zoom-in").addEventListener("click", function () { zoomAt(ZOOM_STEP); });
  document.getElementById("bench-zoom-out").addEventListener("click", function () { zoomAt(1 / ZOOM_STEP); });
  document.getElementById("bench-fit").addEventListener("click", fit);
  undoBtn.addEventListener("click", undo);
  redoBtn.addEventListener("click", redo);
  saveBtn.addEventListener("click", function () { save(); });
  makeLiveBtn.addEventListener("click", makeLive);
  stopLiveBtn.addEventListener("click", stopLive);
  declareBtn.addEventListener("click", function () { writeThrough("Declared again", "Not declared again"); });
  retryBtn.addEventListener("click", function () { liveState = "unknown"; renderLiveBar(); readLive(); });
  wtBtn.addEventListener("click", function () { openLivePanel(wtBtn); });

  cfgEl.addEventListener("click", function (e) {
    var b = closest(e.target, "[data-cfg]");
    if (!b || !bench) return;
    cfgIx = +b.getAttribute("data-cfg");
    laying = null;
    buildModel();
    if (sel && sel.kind === "cable" && !cableById(sel.id)) sel = null;
    renderConfigs();
    render();
    renderInspector();
    renderNotes();
    renderLiveBar();   // a view change: what the live bench offers follows the tab, Core is not called
    scheduleCheck();
  });

  picker.addEventListener("change", function () {
    var want = picker.value, here = bench ? bench.slug : "";
    if (want === NEW_BENCH) {
      picker.value = here;
      confirmLeave("bench").then(function (ok) { if (ok) newBench(); });
      return;
    }
    if (!want || want === here) return;
    lostEdits = null;
    if (isDirty() || partsUnsaved()) {
      picker.value = here;
      confirmLeave("bench").then(function (ok) { if (ok) { picker.value = want; loadBench(want); } });
      return;
    }
    loadBench(want);
  });

  // ---------------------------------------------------------------- keys
  function topologyActive() { return !!document.querySelector('.tab-panel.active[data-tab="topology"]'); }
  function shown(d) {
    return d.style.display === "block" || (!d.hidden && d.style.display !== "none" && d.getClientRects().length > 0);
  }
  function foreignDialog() {
    return Array.prototype.some.call(document.querySelectorAll("#prompt-dialog, .dialog, .desk-dialog"), function (d) {
      return d !== dlg.box && shown(d);
    });
  }
  function inField(el) {
    return !!el && (/^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) || el.isContentEditable);
  }

  // Captured before app.js's own Esc, so a cable being laid, a tray item
  // picked or the bench's dialog is what Esc closes first.
  document.addEventListener("keydown", function (e) {
    if (dlgState) {
      // Core's operator prompt, when it is up, owns Esc.
      var prompt = document.getElementById("prompt-dialog");
      if (e.key === "Escape" && !(prompt && shown(prompt))) { e.preventDefault(); e.stopPropagation(); closeDialog(null); }
      return;
    }
    if (!topologyActive() || !bench || !model || foreignDialog()) return;
    var mod = e.ctrlKey || e.metaKey, k = e.key;
    if (mod && !e.altKey && (k === "s" || k === "S")) {
      e.preventDefault();
      if (document.activeElement && insEl.contains(document.activeElement) && document.activeElement.blur) document.activeElement.blur();
      save();
      return;
    }
    if (k === "Escape") {
      // The topmost thing first (decision 78): a panel open over the canvas
      // is app.js's to close before a cable, a picked tray item or the bin.
      if (document.querySelector(".topo-panel:not([hidden])")) return;
      if (laying) { e.stopPropagation(); cancelLaying(); return; }
      if (trayArm) { e.stopPropagation(); trayArm = null; renderTray(); render(); return; }
      if (!binEl.hidden) { setBin(false); return; }
      return;
    }
    if (inField(e.target) || closest(document.activeElement, ".topo-panel")) return;
    // A cable being laid or a part being dragged was answered for the bench
    // as it stands: nothing changes it under the gesture.
    if (laying || drag) return;
    if (mod && !e.altKey && (k === "z" || k === "Z")) { e.preventDefault(); if (e.shiftKey) redo(); else undo(); return; }
    if (mod && !e.altKey && (k === "y" || k === "Y")) { e.preventDefault(); redo(); return; }
    if (mod || e.altKey) return;
    if (k === "Delete" || k === "Backspace") {
      if (sel) { e.preventDefault(); deleteSelection(); }
      return;
    }
    if ((k === "f" || k === "F") && sel && sel.kind === "part") { e.preventDefault(); flipSelection(); }
  }, true);

  // A reload or a closed tab with unsaved edits: the browser asks.
  window.addEventListener("beforeunload", function (e) {
    if (!isDirty() && !partsUnsaved()) return;
    e.preventDefault();
    e.returnValue = "";
  });

  if (typeof ResizeObserver === "function") {
    new ResizeObserver(function () {
      if (!model) return;
      if (!fitted) fit(); else setViewBox();
    }).observe(wrap);
  } else {
    window.addEventListener("resize", function () { if (model) { if (!fitted) fit(); else setViewBox(); } });
  }

  // ---------------------------------------------------------------- the part editor's preview
  // One template drawn with the canvas's own renderer, for the part editor's
  // live preview: a pure draw into the given `<svg>`, which touches nothing
  // the canvas holds. Every connector is drawn, as if wired.
  function num(v) { var n = Number(v); return isFinite(n) && n > 0 ? n : null; }
  function previewTemplate(svgEl, template) {
    if (!svgEl) return;
    var t = template && typeof template === "object" ? template : {};
    var conns = Array.isArray(t.connectors) ? t.connectors.filter(function (c) { return c && typeof c === "object"; }) : [];
    var tpl = { id: String(t.id || "preview"), label: String(t.label || t.id || ""), subtitle: t.subtitle || null,
                category: t.category || null, look: t.look || null, source: null, hub: !!t.hub,
                width: num(t.width) || DEFAULT_WIDTH, min_height: num(t.min_height), rows_top: num(t.rows_top),
                connectors: conns, passthrough: [] };
    var p = { key: tpl.id, id: tpl.id, tpl: tpl, missing: false, unreadable: null, dup: 0, conns: [], byId: dict(),
              inst: { id: tpl.id, template: tpl.id, label: null, at: [0, 0], role: null, mounted_on: null, flip: false, usb: {} } };
    conns.forEach(function (c) {
      var conn = connOf({ id: String(c.id == null ? "" : c.id), label: c.label == null ? "" : String(c.label), kind: c.kind,
                          side: c.side, power: c.power, functions: Array.isArray(c.functions) ? c.functions : [] });
      p.conns.push(conn);
      p.byId[conn.id] = conn;
    });
    var keep = [model, cache, sel, hoverPart, laying, drag, trayArm, trayDrag, trayHover, previewing];
    var body = "", g = null;
    try {
      model = { parts: dict(), order: [p], cables: [], notes: [] };
      model.parts[p.key] = p;
      cache = { vis: dict(), geo: dict(), below: dict() };
      sel = null; hoverPart = null; laying = null; drag = null; trayArm = null; trayDrag = null; trayHover = null;
      previewing = true;
      body = partSVG(p);
      g = geom(p);
    } finally {
      model = keep[0]; cache = keep[1]; sel = keep[2]; hoverPart = keep[3]; laying = keep[4]; drag = keep[5];
      trayArm = keep[6]; trayDrag = keep[7]; trayHover = keep[8]; previewing = keep[9];
    }
    var padX = 70, padTop = 34, padBottom = 36;
    svgEl.classList.add("bn-svg");
    svgEl.setAttribute("viewBox", f(g.x - padX) + " " + f(g.y - padTop) + " " + f(g.w + padX * 2) + " " + f(g.h + padTop + padBottom));
    svgEl.innerHTML = body;
  }

  window.embarchBench = {
    previewTemplate: previewTemplate,
    unsaved: function () { return isDirty() || partsUnsaved(); },
    confirmLeave: confirmLeave,
    leaveDone: leaveDone,
    // The parts of the bench on screen a `#topology?board=` link marks, and
    // whether a load has finished — the board types' served labels read
    // too, when a bench is on screen — so "none" means none.
    markedParts: function (board) { return linkMatches(board).slice(); },
    // Saved benches' Open and the desk banner: that bench on the canvas,
    // Make live for the human to press.
    showBench: showBench,
    settled: function () { return settledOnce && (!model || !!pickers || !!pickersError); }
  };

  // A project switch reloads — the parts and benches are the open repo's — and
  // so does a save or delete on the Saved benches panel, which writes the same
  // files. A load already in flight is discarded by its sequence number.
  // A switch made here asked first (app.js); one made elsewhere — a desk
  // opened in another window — could not, and a note under the canvas says
  // what went. The first read of the project at page load is no switch.
  document.addEventListener("embarch:project", function (ev) {
    var d = (ev && ev.detail) || {};
    closeDialog(null);
    pickers = null; pickersError = null; pickersAsked = false;   // the board types are the repo's
    if (d.initial && bench) return;
    var asked = leavePending === "project" || leavePending === "desk";
    lostEdits = !asked && isDirty() ? "The project switched with unsaved changes to " + (bench.name || bench.slug) + " on screen; they were not saved." : null;
    if (asked) leaveLanded(); else leaveDone(false);
    // The bench on screen is the old repo's: it goes now, so nothing can save
    // it into the new one, and a bench the new repo cannot read is said as
    // that, never shown as the old one.
    clearBench();
    // What the old repo's files said, and what Validate said about its bench,
    // are not the new repo's: they go before anything is drawn.
    vocab = null; templates = dict(); unreadable = dict(); partErrors = []; benchErrors = []; benchList = [];
    verdicts = { roles: dict(), boards: dict() }; homes = dict(); verdictDrops = [];
    projectGen++; liveSeq++; readSeq++; settledOnce = false;
    live = null; superseded = null; wt = null; liveState = "unknown"; liveReport = null; liveRows = dict();
    liveBusy = false; liveWrites = 0; readDropped = false; liveOpener = null; pendingOffer = null;
    if (liveBtn.classList.contains("is-open")) liveBtn.click();
    renderLiveReport();
    renderLiveBar();
    renderBin();
    picker.hidden = true;
    showState("reading…", false);
    renderNotes();
    loadAll(false);
  });
  document.addEventListener("embarch:benches-changed", function () { loadAll(true); });
  renderBin();
  loadAll(false);
})();
