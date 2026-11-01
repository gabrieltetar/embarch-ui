// embarch-ui bench canvas: a saved bench drawn as the hardware on the shelf.
//
// Phase 2 of embarch-bench-editor-proposal.md, read-only, beside the Topology
// tab's old diagram. Everything drawn comes from the UI server's bench routes
// (`/api/bench/*`): the vocabulary, the part templates and the bench files. A
// template's `look` picks the drawing and a connector's `kind` picks its
// glyph; a look or kind this file has no drawing for is drawn as a plain
// board or a plain port, never dropped.
//
// The live glow is the one thing taken from the Topology snapshot (app.js
// re-dispatches each one as `embarch:snapshot`), and **a snapshot only
// toggles classes**: the canvas is rebuilt by a gesture or a load, never by a
// poll, so a selection or a hover survives it. Decision 43's properties carry
// over: every listener lives on the `<svg>`, and what the pointer marked is
// state re-applied on every rebuild rather than waited for.
//
// Zero-build (decision 2): plain JS, one SVG, no library, no timer.

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

  var ROUTE_VOCAB = "/api/bench/vocabulary";
  var ROUTE_PARTS = "/api/bench/parts";
  var ROUTE_BENCHES = "/api/bench/benches";
  var BENCH_KEY = "embarch-ui.bench";

  var HEAD = 46;            // a part's title band, above its first connector row
  var ROW = 24;             // one connector row
  var FOOT = 10;            // below the last row
  var TOP_STEP = 40;        // between two connectors on a part's top edge
  var DEFAULT_WIDTH = 180;  // a part whose template is gone or gives none
  var MONO_ADVANCE = 6.6;   // px per character, 11px Plex Mono (0.6 em, decision 42)
  var SANS_ADVANCE = 7.4;   // px per character, 13px Plex Sans semibold, roughly
  var ZOOM_MIN = 0.3, ZOOM_MAX = 2.5, FIT_MAX = 1.4, FIT_PAD = 60, ZOOM_STEP = 1.2;

  // ---------------------------------------------------------------- state
  var vocab = null;          // GET /api/bench/vocabulary
  var templates = {};        // template id -> template, GET /api/bench/parts
  var fileErrors = [];       // the files those two lists could not read
  var benchList = [];
  var bench = null;          // the bench on screen, GET /api/bench/benches/{slug}
  var model = null;          // bench + the config on screen, resolved into parts and cables
  var cfgIx = 0;
  var sel = null;            // { kind: "part" | "cable", id }
  var hoverPart = null;
  var attached = {};         // probe serial -> true, from the latest snapshot
  var view = { x: 0, y: 0, k: 1 };
  var fitted = false;
  var pan = null;
  var cache = null;          // per-render geometry
  var loadSeq = 0, benchSeq = 0;

  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }
  function f(v) { return Math.round(v * 10) / 10; }
  function clamp(v, a, b) { return Math.max(a, Math.min(b, v)); }
  function closest(el, sel) { return el && el.closest ? el.closest(sel) : null; }
  function cut(text, max) {
    text = String(text == null ? "" : text);
    max = Math.max(3, Math.floor(max));
    return text.length > max ? text.slice(0, max - 1) + "…" : text;
  }

  // ---------------------------------------------------------------- the vocabulary
  // Read, never restated: which kind reaches a hub, and which kinds are never
  // cabled (a stack socket or plug joins through `mounted_on`, so it is in no
  // `joins` pair and is not the hub's kind).
  function hubKind() { return vocab && vocab.hub_kind; }
  function neverCabled(kind) {
    if (!vocab || (vocab.kinds || []).indexOf(kind) < 0 || kind === vocab.hub_kind) return false;
    return !(vocab.joins || []).some(function (pair) { return pair.indexOf(kind) >= 0; });
  }

  // ---------------------------------------------------------------- the model
  function currentConfig() {
    var cfgs = (bench && bench.configs) || [];
    return cfgs[cfgIx] || null;
  }

  function name(p) { return p.inst.label || p.tpl.label || p.id; }

  // A part whose template is gone is drawn, never dropped (decision 44): a
  // plain board under its own id, its connectors whatever its cables name.
  function missingTemplate(inst) {
    return { id: inst.template, label: inst.template, subtitle: null, category: null, look: null,
             source: null, hub: false, width: DEFAULT_WIDTH, min_height: null, rows_top: null,
             connectors: [], passthrough: [] };
  }

  function buildModel() {
    var m = { parts: {}, order: [], cables: [], notes: [] };
    (bench.parts || []).forEach(function (inst) {
      var tpl = templates[inst.template];
      var p = { id: inst.id, inst: inst, tpl: tpl || missingTemplate(inst), missing: !tpl, conns: [], byId: {} };
      (p.tpl.connectors || []).forEach(function (c) {
        var conn = { id: c.id, label: c.label || c.id, kind: c.kind, side: c.side, power: c.power,
                     functions: c.functions || [], stack: neverCabled(c.kind), tpl: c };
        p.conns.push(conn);
        p.byId[c.id] = conn;
      });
      m.parts[inst.id] = p;
      m.order.push(p);
    });
    (bench.cables || []).forEach(function (c, i) { addCable(m, c.from, c.to, "bench", "b" + i); });
    var cfg = currentConfig();
    ((cfg && cfg.cables) || []).forEach(function (pair, i) { addCable(m, pair[0], pair[1], "config", "c" + i); });
    placeUnplaced(m);
    sideMissing(m);
    hubPorts(m);
    model = m;
  }

  function addCable(m, from, to, level, id) {
    var a = resolveEnd(m, from), b = resolveEnd(m, to);
    if (!a || !b) {
      var gone = !a ? from : to;
      m.notes.push("cable " + from + " ↔ " + to + ": no part " + String(gone).split(".")[0] + " on this bench");
      return;
    }
    m.cables.push({ id: id, level: level, a: a, b: b, from: from, to: to });
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

  // A part with no position and no base to sit on goes in a row under the rest.
  function placeUnplaced(m) {
    var bottom = 0, row = 0;
    m.order.forEach(function (p) { if (p.inst.at) bottom = Math.max(bottom, p.inst.at[1] + 160); });
    m.order.forEach(function (p) {
      if (p.inst.at || (p.inst.mounted_on && m.parts[p.inst.mounted_on])) return;
      p.fallbackAt = [20 + row * 220, bottom + 40];
      row++;
    });
  }

  // Where a part's file puts it: its own `at`, its row under the rest, or the
  // `at` of what it is mounted on.
  function rawAt(m, p, depth) {
    if (p.inst.at) return p.inst.at;
    if (p.fallbackAt) return p.fallbackAt;
    var base = p.inst.mounted_on && (depth || 0) < 8 ? m.parts[p.inst.mounted_on] : null;
    return base ? rawAt(m, base, (depth || 0) + 1) : [0, 0];
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
  // reaches it, named by the part at the far end — a probe's by what its other
  // cable reaches — and ordered top to bottom by that part so cables do not cross.
  function hubPorts(m) {
    var ports = [];
    m.cables.forEach(function (cb) {
      [[cb.a, cb.b], [cb.b, cb.a]].forEach(function (pair) {
        var end = pair[0], far = pair[1];
        if (!end.hub) return;
        var port = { id: "~" + cb.id, label: hubLabel(m, far), kind: hubKind(), side: "right",
                     hubPort: true, cable: cb, far: far, functions: [] };
        end.conn = port;
        ports.push({ hub: end.part, port: port });
      });
    });
    ports.sort(function (x, y) {
      return rawAt(m, x.port.far.part)[1] - rawAt(m, y.port.far.part)[1];
    });
    ports.forEach(function (e) { e.hub.conns.push(e.port); e.hub.byId[e.port.id] = e.port; });
  }

  function hubLabel(m, far) {
    var p = far.part;
    if (p.tpl.category !== "probe") return name(p);
    var reach = null;
    m.cables.some(function (cb) {
      var mine = cb.a.part === p ? cb.a : cb.b.part === p ? cb.b : null;
      if (!mine || mine.hub || !mine.conn || mine.conn.kind === hubKind()) return false;
      var other = mine === cb.a ? cb.b : cb.a;
      if (other.part === p) return false;
      reach = other.part;
      return true;
    });
    return reach ? name(p) + " → " + name(reach) : name(p);
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

  // The serial a USB connector is bound to, when it is the hub's kind and the
  // part binds a probe on it: the only thing the glow reads.
  function boundProbe(p, c) {
    if (!c || c.kind !== hubKind()) return null;
    var usb = p.inst.usb || {};
    var b = usb[c.id];
    return b && b.probe ? String(b.probe) : null;
  }
  function cableProbe(cb) {
    return boundProbe(cb.a.part, cb.a.conn) || boundProbe(cb.b.part, cb.b.conn);
  }

  // ---------------------------------------------------------------- geometry
  function sideOf(p, c) {
    var s = c.side === "right" || c.side === "top" ? c.side : "left";
    if (p.inst.flip && s !== "top") s = s === "left" ? "right" : "left";
    return s;
  }

  function isWired(p, c) { return cablesAt(p, c).length > 0; }

  // **A part shows only its wired connectors**; its free ones appear on intent,
  // hovered or selected. Wired rows come first on every side, so revealing the
  // free ones never moves a cable.
  function visibleConns(p) {
    if (cache && cache.vis[p.id]) return cache.vis[p.id];
    var showAll = hoverPart === p.id || (!!sel && sel.kind === "part" && sel.id === p.id);
    var wired = [], free = [];
    p.conns.forEach(function (c) {
      if (isWired(p, c)) wired.push(c);
      else if (!c.stack && showAll) free.push(c);
    });
    var out = { list: wired.concat(free), free: free };
    if (cache) cache.vis[p.id] = out;
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

  function geom(p, depth) {
    if (cache && cache.geo[p.id]) return cache.geo[p.id];
    var w = p.tpl.width || DEFAULT_WIDTH, h = partH(p), g;
    var base = p.inst.mounted_on && (depth || 0) < 8 ? model.parts[p.inst.mounted_on] : null;
    if (base) {
      var bg = geom(base, (depth || 0) + 1);
      g = { x: bg.x, y: bg.y - h, w: w, h: h };
    } else {
      var at = rawAt(model, p);
      g = { x: at[0], y: at[1], w: w, h: h };
    }
    if (cache) cache.geo[p.id] = g;
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
    return shadow(g, 5) + bodyRect(g, 5, cls + (p.missing ? " bn-body-missing" : "")) + (g.h >= 40 ? holes(g) : "");
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
    // the first power role its connectors carry, verbatim.
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
    // A probe: a pod.
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
    // The generic look: a board.
    board: function (p, g) {
      var s = pcb(p, g, "bn-pcb");
      if (hasRows(p, g)) s += chip(quietSide(p) === "right" ? g.x + g.w - 44 : g.x + 22, rowMid(p, g) - 10, 20);
      return onPcb(s);
    }
  };
  function lookOf(p) {
    return !p.missing && Object.prototype.hasOwnProperty.call(LOOKS, p.tpl.look) ? p.tpl.look : "board";
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
  function partSubtitle(p) {
    if (p.missing) return "template missing · " + p.inst.template;
    var shields = model.order.filter(function (q) { return q.inst.mounted_on === p.id; });
    if (shields.length) return p.tpl.label + " + " + shields.map(name).join(" + ");
    if (p.inst.label && p.inst.label !== p.tpl.label) return p.tpl.label;
    return p.tpl.subtitle || "";
  }

  function partSVG(p) {
    var g = geom(p), look = lookOf(p), vis = visibleConns(p);
    var isSel = !!sel && sel.kind === "part" && sel.id === p.id;
    var deco = LOOKS[look](p, g);
    var s = '<g class="bn-part' + (isSel ? " bn-sel" : "") + (hoverPart === p.id ? " bn-hover" : "") +
      (p.missing ? " bn-part-missing" : "") + '" data-part="' + esc(p.id) + '" data-look="' + look + '" data-template="' + esc(p.inst.template) + '">';
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
    if (p.inst.mounted_on && model.parts[p.inst.mounted_on]) {
      var pins = '<g class="bn-pins">';
      for (var i = 0; g.x + 22 + i * 18 + 8 <= g.x + g.w - 14; i++) pins += rect("", g.x + 22 + i * 18, g.y + g.h - 3, 8, 6, 1);
      s += pins + "</g>";
    }
    vis.list.forEach(function (c) {
      var pos = connPos(p, c), free = vis.free.indexOf(c) >= 0;
      var cables = cablesAt(p, c).map(function (cb) { return cb.id; });
      var probe = c.hubPort ? cableProbe(c.cable) : boundProbe(p, c);
      var title = c.label + (c.kind ? " · " + c.kind : "") + (c.power ? " · " + c.power : "") +
        (c.missing ? " · not on " + p.tpl.label : "");
      s += '<g class="bn-port' + (free ? " bn-free" : " bn-wired") + (c.missing ? " bn-port-missing" : "") +
        '" data-conn="' + esc(p.id + "." + c.id) + '" data-kind="' + esc(c.kind || "") + '" data-x="' + f(pos.x) + '" data-y="' + f(pos.y) + '"' +
        (cables.length ? ' data-cables="' + esc(cables.join(" ")) + '"' : "") + (c.hubPort ? " data-hub-port" : "") + ">" +
        circle("bn-hit", pos.x, pos.y, 12) + portGlyph(c, pos, look) + "<title>" + esc(title) + "</title></g>";
      s += labelSVG(c, pos, g, free, deco);
      if (probe) s += '<circle class="bn-livering" data-probe="' + esc(probe) + '" cx="' + f(pos.x) + '" cy="' + f(pos.y) + '" r="11"/>';
    });
    return s + "</g>";
  }

  function render() {
    if (!bench || !model) return;
    cache = { vis: {}, geo: {} };
    var s = '<defs><pattern id="bench-dots" width="24" height="24" patternUnits="userSpaceOnUse">' +
      '<circle class="bn-grid-dot" cx="1" cy="1" r="1"/></pattern></defs>' +
      '<rect id="bench-grid" fill="url(#bench-dots)" x="0" y="0" width="10" height="10"/>';
    model.cables.forEach(function (cb) {
      var d = cablePath(connPos(cb.a.part, cb.a.conn), connPos(cb.b.part, cb.b.conn));
      var kindEnd = cb.a.hub ? cb.b : cb.a;
      var kind = (kindEnd.conn && kindEnd.conn.kind) || "";
      var probe = cableProbe(cb);
      var isSel = !!sel && sel.kind === "cable" && sel.id === cb.id;
      if (probe) s += '<path class="bn-glow" data-probe="' + esc(probe) + '" d="' + d + '"/>';
      s += '<path class="bn-cable' + (cb.level === "config" ? " bn-cfg" : " bn-bench") + (isSel ? " bn-sel" : "") +
        '" data-cable="' + cb.id + '" data-kind="' + esc(kind) + '" d="' + d + '"/>';
      s += '<path class="bn-cable-hit" data-cable-hit="' + cb.id + '" d="' + d + '"><title>' +
        esc(endName(cb.a) + " ↔ " + endName(cb.b)) + "</title></path>";
    });
    // Bases first, so a mounted part sits on top of the one it is mounted on.
    var depth = function (p) {
      var n = 0, q = p;
      while (q && q.inst.mounted_on && model.parts[q.inst.mounted_on] && n < 8) { q = model.parts[q.inst.mounted_on]; n++; }
      return n;
    };
    model.order.slice().sort(function (a, b) { return depth(a) - depth(b); }).forEach(function (p) { s += partSVG(p); });
    svg.innerHTML = s;
    setViewBox();
    applyLive();
  }

  // ---------------------------------------------------------------- the live glow
  // **A `usb-host` connector glows when its bound probe is enumerated now** —
  // read off the Topology snapshot and nothing else. Toggling classes is all a
  // snapshot does here.
  function applyLive() {
    [svg, insEl].forEach(function (root) {
      root.querySelectorAll("[data-probe]").forEach(function (el) {
        el.classList.toggle("bn-live", !!attached[el.getAttribute("data-probe")]);
      });
    });
  }

  document.addEventListener("embarch:snapshot", function (ev) {
    var snap = ev.detail || {};
    var now = {};
    (snap.probes || []).forEach(function (p) { if (p && p.serial_number) now[String(p.serial_number)] = true; });
    attached = now;
    applyLive();
  });

  // ---------------------------------------------------------------- the inspector
  function kindCell(c) {
    var fns = (c.functions || []).map(function (fn) {
      var bits = [fn.function];
      for (var k in fn) if (k !== "function" && fn[k] != null) bits.push(k + " " + fn[k]);
      return bits.join(" ");
    });
    return [c.kind || "?"].concat(c.power ? [c.power] : []).join(" · ") + (fns.length ? " · " + fns.join(", ") : "");
  }

  function reaches(p, c) {
    if (c.stack) {
      var on = model.order.filter(function (q) { return q.inst.mounted_on === p.id; }).map(name);
      if (on.length) return on.join(", ");
      var base = p.inst.mounted_on && model.parts[p.inst.mounted_on];
      return base ? name(base) : "";
    }
    return cablesAt(p, c).map(function (cb) { return endName(otherEnd(cb, p, c)); }).join(", ");
  }

  function partInspector(p) {
    var src = p.missing ? "" : p.tpl.source === "repo" ? "embarch/parts/" + p.tpl.id + ".toml" : "built in";
    var h = '<div class="bn-ins-title">' + esc(name(p)) + "</div>";
    h += p.missing
      ? '<div class="sd-error">template ' + esc(p.inst.template) + " missing</div>"
      : '<div class="bn-ins-meta">' + esc(p.tpl.label) + " · " + esc(src) + "</div>";
    var kv = [["Role", p.inst.role || "none"]];
    if (p.inst.board) kv.push(["Board", p.inst.board]);
    var base = p.inst.mounted_on && model.parts[p.inst.mounted_on];
    if (p.inst.mounted_on) kv.push(["On", base ? name(base) : p.inst.mounted_on + " (not on this bench)"]);
    var carried = model.order.filter(function (q) { return q.inst.mounted_on === p.id; }).map(name);
    if (carried.length) kv.push(["Carries", carried.join(", ")]);
    h += '<dl class="bn-kv">' + kv.map(function (r) { return "<dt>" + esc(r[0]) + "</dt><dd>" + esc(r[1]) + "</dd>"; }).join("") + "</dl>";
    var rows = p.conns.map(function (c) {
      var r = reaches(p, c);
      return '<tr data-conn-row="' + esc(c.id) + '"><td class="bn-k" data-kind="' + esc(c.kind || "") + '">' + esc(c.label) +
        (c.missing ? ' <span class="bn-missing-text">not on the template</span>' : "") + "</td>" +
        '<td class="mono">' + esc(kindCell(c)) + "</td><td>" + (r ? esc(r) : '<span class="bn-muted">—</span>') + "</td></tr>";
    }).join("");
    if (rows) h += '<table class="bn-conns"><tbody>' + rows + "</tbody></table>";
    var usb = p.inst.usb || {};
    Object.keys(usb).forEach(function (cid) {
      var b = usb[cid] || {}, c = p.byId[cid];
      var bits = [];
      for (var k in b) if (k !== "probe" && b[k] != null) bits.push(k + " " + b[k]);
      h += '<div class="bn-usb"' + (b.probe ? ' data-probe="' + esc(b.probe) + '"' : "") + "><span>" + esc(c ? c.label : cid) + "</span> " +
        '<span class="mono">' + (b.probe ? "probe " + esc(b.probe) : "") + (bits.length ? (b.probe ? " · " : "") + esc(bits.join(" · ")) : "") + "</span>" +
        (b.probe ? '<span class="bn-enum">enumerated</span>' : "") + "</div>";
    });
    return h;
  }

  function cableInspector(cb) {
    var kindEnd = cb.a.hub ? cb.b : cb.a;
    var kind = (kindEnd.conn && kindEnd.conn.kind) || "?";
    var cfg = currentConfig();
    var probe = cableProbe(cb);
    var h = '<div class="bn-ins-title">' + esc(kind) + " cable</div>" +
      '<div class="bn-ins-meta">' + esc(endName(cb.a)) + " ↔ " + esc(endName(cb.b)) + "</div>";
    var kv = '<dt>Config</dt><dd>' + esc(cb.level === "bench" ? "every config" : (cfg ? cfg.name : "")) + "</dd>";
    if (probe) kv += '<dt>Probe</dt><dd data-probe="' + esc(probe) + '"><span class="mono">' + esc(probe) + '</span><span class="bn-enum">enumerated</span></dd>';
    return h + '<dl class="bn-kv">' + kv + "</dl>";
  }

  function renderInspector() {
    if (!bench || !model) { insEl.innerHTML = ""; return; }
    var cb = sel && sel.kind === "cable" ? cableById(sel.id) : null;
    if (sel && sel.kind === "part" && model.parts[sel.id]) insEl.innerHTML = partInspector(model.parts[sel.id]);
    else if (cb) insEl.innerHTML = cableInspector(cb);
    else insEl.innerHTML = '<div class="bn-ins-title">' + esc(bench.name || bench.slug) + '</div><p class="bn-muted">select a part or a cable</p>';
    applyLive();
  }

  // ---------------------------------------------------------------- the frame
  function renderConfigs() {
    var cfgs = (bench && bench.configs) || [];
    cfgEl.innerHTML = cfgs.map(function (c, i) {
      return '<button type="button" class="bench-cfg" role="tab" aria-selected="' + (i === cfgIx) + '" data-cfg="' + i + '">' + esc(c.name) + "</button>";
    }).join("");
    cfgRow.hidden = !cfgs.length;
  }

  function renderNotes() {
    var lines = fileErrors.map(function (e) {
      return (e && e.file ? e.file : "a file") + ": " + (e && e.error ? e.error : "unreadable");
    }).concat(model && bench ? model.notes : []);
    notesEl.innerHTML = lines.map(function (l) { return "<li>" + esc(l) + "</li>"; }).join("");
    notesEl.hidden = !lines.length;
  }

  function showState(text, isError) {
    stateEl.textContent = text;
    stateEl.className = isError ? "sd-error bench-state" : "placeholder-note bench-state";
    stateEl.hidden = !text;
  }

  // The routes answered 404: this server has no bench routes. One quiet line —
  // never a banner, and never an empty bench standing in for a real one.
  function showUnavailable() {
    bench = null; model = null; vocab = null; templates = {}; fileErrors = []; benchList = [];
    svg.innerHTML = "";
    bodyEl.hidden = true;
    picker.hidden = true;
    pathEl.hidden = true;
    notesEl.hidden = true;
    showState("bench routes not available on this server", false);
  }

  function showFailure(e) {
    bench = null; model = null;
    svg.innerHTML = "";
    bodyEl.hidden = true;
    showState((e && e.route ? e.route + ": " : "") + (e && e.message ? e.message : String(e)), true);
    renderNotes();
  }

  function api(route) {
    return fetch(route, { headers: { Accept: "application/json" } }).then(function (r) {
      if (r.ok) return r.json();
      return r.text().then(function (t) {
        var err = new Error(t || "HTTP " + r.status);
        err.status = r.status;
        err.route = route;
        throw err;
      });
    });
  }

  function remembered() {
    try { return localStorage.getItem(BENCH_KEY); } catch (_) { return null; }
  }
  function remember(slug) {
    try { localStorage.setItem(BENCH_KEY, slug); } catch (_) { /* per-viewer only */ }
  }

  function loadAll() {
    var seq = ++loadSeq;
    Promise.all([api(ROUTE_VOCAB), api(ROUTE_PARTS), api(ROUTE_BENCHES)]).then(function (res) {
      if (seq !== loadSeq) return;
      vocab = res[0] || {};
      templates = {};
      ((res[1] && res[1].parts) || []).forEach(function (t) { templates[t.id] = t; });
      fileErrors = ((res[1] && res[1].errors) || []).concat((res[2] && res[2].errors) || []);
      benchList = (res[2] && res[2].benches) || [];
      picker.innerHTML = benchList.map(function (b) {
        return '<option value="' + esc(b.slug) + '">' + esc(b.name || b.slug) + "</option>";
      }).join("");
      picker.hidden = !benchList.length;
      if (!benchList.length) {
        bench = null; model = null;
        bodyEl.hidden = true;
        pathEl.hidden = false;
        pathEl.textContent = "embarch/topologies/";
        // A list emptied by files it could not read is not "no benches".
        showState(fileErrors.length ? "no saved bench could be read" : "no saved benches", false);
        renderNotes();
        return;
      }
      var want = remembered();
      var slug = benchList.some(function (b) { return b.slug === want; }) ? want : benchList[0].slug;
      picker.value = slug;
      loadBench(slug);
    }).catch(function (e) {
      if (seq !== loadSeq) return;
      if (e && e.status === 404) showUnavailable();
      else showFailure(e);
    });
  }

  function loadBench(slug) {
    var seq = ++benchSeq;
    api(ROUTE_BENCHES + "/" + encodeURIComponent(slug)).then(function (b) {
      if (seq !== benchSeq) return;
      bench = b || {};
      cfgIx = 0; sel = null; hoverPart = null; fitted = false;
      remember(slug);
      buildModel();
      showState("", false);
      pathEl.hidden = false;
      pathEl.textContent = "embarch/topologies/" + (bench.slug || slug) + ".toml" +
        (bench.legacy ? " · no [[parts]]: laid out from its dut and dev-bench" : "");
      bodyEl.hidden = false;
      renderConfigs();
      render();
      renderInspector();
      renderNotes();
      fit();
    }).catch(function (e) {
      if (seq !== benchSeq) return;
      showFailure(e);
    });
  }

  // ---------------------------------------------------------------- view
  function wrapSize() {
    var r = wrap.getBoundingClientRect();
    return { w: r.width, h: r.height, left: r.left, top: r.top };
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

  function fit() {
    if (!model || !model.order.length) return;
    var r = wrapSize();
    if (r.w < 20 || r.h < 20) return;   // a hidden tab has no size yet; the resize observer fits it later
    cache = cache || { vis: {}, geo: {} };
    var x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    model.order.forEach(function (p) {
      var g = geom(p);
      x0 = Math.min(x0, g.x); y0 = Math.min(y0, g.y - 20);
      x1 = Math.max(x1, g.x + g.w); y1 = Math.max(y1, g.y + g.h + 26);
    });
    var bw = x1 - x0 + FIT_PAD * 2, bh = y1 - y0 + FIT_PAD * 2;
    view.k = clamp(Math.min(r.w / bw, r.h / bh), ZOOM_MIN, FIT_MAX);
    view.x = x0 - FIT_PAD - Math.max(0, (r.w / view.k - bw) / 2);
    view.y = y0 - FIT_PAD - Math.max(0, (r.h / view.k - bh) / 2);
    fitted = true;
    setViewBox();
  }

  // ---------------------------------------------------------------- gestures
  function select(next) {
    sel = next;
    render();
    renderInspector();
  }

  svg.addEventListener("pointerdown", function (e) {
    if (e.button !== 0 || !model) return;
    var port = closest(e.target, "[data-conn]");
    var part = closest(e.target, "[data-part]");
    var hit = closest(e.target, "[data-cable-hit]");
    if (port && port.getAttribute("data-cables")) {
      select({ kind: "cable", id: port.getAttribute("data-cables").split(" ")[0] });
      return;
    }
    if (part) { select({ kind: "part", id: part.getAttribute("data-part") }); return; }
    if (hit) { select({ kind: "cable", id: hit.getAttribute("data-cable-hit") }); return; }
    pan = { sx: e.clientX, sy: e.clientY, vx: view.x, vy: view.y, moved: false, id: e.pointerId };
    try { svg.setPointerCapture(e.pointerId); } catch (_) { /* a synthetic pointer has none */ }
  });

  svg.addEventListener("pointermove", function (e) {
    if (pan) {
      var dx = e.clientX - pan.sx, dy = e.clientY - pan.sy;
      if (!pan.moved && Math.abs(dx) + Math.abs(dy) < 4) return;
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
    try { svg.releasePointerCapture(pan.id); } catch (_) { /* already released */ }
    pan = null;
    if (clicked && e.type === "pointerup" && sel) select(null);
  }
  svg.addEventListener("pointerup", endPan);
  svg.addEventListener("pointercancel", endPan);
  svg.addEventListener("pointerleave", function () {
    if (!pan && hoverPart) { hoverPart = null; render(); }
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

  cfgEl.addEventListener("click", function (e) {
    var b = closest(e.target, "[data-cfg]");
    if (!b || !bench) return;
    cfgIx = +b.getAttribute("data-cfg");
    buildModel();
    if (sel && sel.kind === "cable" && !cableById(sel.id)) sel = null;
    renderConfigs();
    render();
    renderInspector();
    renderNotes();
  });

  picker.addEventListener("change", function () { if (picker.value) loadBench(picker.value); });

  if (typeof ResizeObserver === "function") {
    new ResizeObserver(function () {
      if (!model) return;
      if (!fitted) fit(); else setViewBox();
    }).observe(wrap);
  } else {
    window.addEventListener("resize", function () { if (model) { if (!fitted) fit(); else setViewBox(); } });
  }

  // A project switch reloads: the parts and benches are the open repo's. The
  // first event only reports the project the load below already read.
  var projectHeard = false;
  document.addEventListener("embarch:project", function () {
    if (projectHeard) loadAll();
    projectHeard = true;
  });
  loadAll();
})();
