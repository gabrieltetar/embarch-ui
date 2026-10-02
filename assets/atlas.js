// embarch-ui Atlas tab: the open project's hardware atlas as one stacked map —
// the firmware's modules on top, the MCU's peripherals and pins under them, and
// every part on every board drawn as a generic schematic symbol with its signals.
//
// Everything drawn comes from the atlas's own graph.json (embarch-atlas writes
// it): the layer names, kinds, statuses, clusters, positions, symbols and
// citations are served, never restated here. The tab is read-only and makes
// no Core call. Zero-build (decision 2): plain JS, one SVG, no library.

(function () {
  "use strict";

  var NS = "http://www.w3.org/2000/svg";
  var panel = document.querySelector('.tab-panel[data-tab="atlas"]');
  if (!panel) return;
  var svg = document.getElementById("atlas-map");
  var wrap = document.getElementById("atlas-wrap");
  var ins = document.getElementById("atlas-inspector");
  var picker = document.getElementById("atlas-pick");
  var stateEl = document.getElementById("atlas-state");
  var emptyEl = document.getElementById("atlas-empty");
  var content = panel.closest(".content");

  var GUT = 150;               // the gutter left of the planes, where layer names sit
  var GAP = 70;                // between flat bands
  var TILT_E = 36 * Math.PI / 180;

  // ---------------------------------------------------------------- state
  var index = null, atlasId = null, G = null, stale = true;
  var LAYERS = [], NL = 0, VOC = {};
  var nodes = [], byId = {}, edges = [], adj = {}, nets = {}, pinsOfNet = {};
  var depth = [], off = [], Zs = [], sceneW = 0, bands = [], STUB = 8, FONT = 6.5;
  var T = 0, cam = { x: 0, y: 0, z: 1 }, zMin = 0.02, zMax = 14;
  var sel = null, selNet = null, selPin = null, hidden = {}, problemIx = -1, problems = [];
  var problemOf = {}, lastZ = -1, rafPending = false, pageDpi = 110, hubParts = {};
  var root, layerEls = [], clusterEls = [], bandEls = [], wireG, pulse, codeNodes = [], billboards = [];

  function el(tag, attrs, parent) {
    var e = document.createElementNS(NS, tag);
    for (var k in attrs) if (attrs[k] !== undefined && attrs[k] !== null) e.setAttribute(k, attrs[k]);
    if (parent) parent.appendChild(e);
    return e;
  }
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }
  function lerp(a, b, t) { return a + (b - a) * t; }
  function ease(t) { return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2; }
  function clamp(v, a, b) { return Math.max(a, Math.min(b, v)); }
  function fx(v) { return (+v).toFixed(1); }

  function api(url) {
    return fetch(url).then(function (r) {
      if (r.ok) return r;
      return r.text().then(function (t) { throw new Error(t || ("HTTP " + r.status)); });
    });
  }

  // ---------------------------------------------------------------- loading
  function showEmpty(msg) {
    emptyEl.textContent = msg;
    emptyEl.style.display = msg ? "" : "none";
  }

  function loadIndex() {
    stale = false;
    showEmpty("");
    api("/api/atlas").then(function (r) { return r.json(); }).then(function (ix) {
      index = ix;
      pageDpi = ix.page_dpi || pageDpi;
      picker.innerHTML = "";
      ix.atlases.forEach(function (a) {
        var o = document.createElement("option");
        o.value = a.id;
        o.textContent = a.id + (a.created ? " · " + a.created.slice(0, 10) : "") + (a.graph ? "" : " · no graph.json");
        o.disabled = !a.graph;
        picker.appendChild(o);
      });
      if (!ix.atlases.length) {
        clearMap();
        showEmpty("no atlas under " + ix.project + "/embarch/atlas/atlases");
        return;
      }
      var want = atlasId && ix.atlases.some(function (a) { return a.id === atlasId && a.graph; }) ? atlasId : ix.default;
      if (!want) {
        clearMap();
        showEmpty("no atlas here has a graph.json — run embarch-atlas graph <atlas>");
        return;
      }
      picker.value = want;
      loadAtlas(want);
    }).catch(function (e) {
      clearMap();
      picker.innerHTML = "";
      stateEl.textContent = "";
      showEmpty(e.message);
    });
  }

  function loadAtlas(id) {
    showEmpty("");
    api("/api/atlas/" + encodeURIComponent(id) + "/graph").then(function (r) { return r.json(); }).then(function (g) {
      atlasId = id;
      var a = index.atlases.filter(function (x) { return x.id === id; })[0] || {};
      stateEl.textContent = a.head ? "at HEAD" : (index.head ? "HEAD " + index.head.slice(0, 7) + (a.behind != null ? " · " + a.behind + " commits past" : " · not an ancestor") : "");
      build(g);
    }).catch(function (e) { clearMap(); showEmpty(e.message); });
  }

  picker.addEventListener("change", function () { loadAtlas(picker.value); });

  function clearMap() {
    while (svg.firstChild) svg.removeChild(svg.firstChild);
    G = null; nodes = []; byId = {}; edges = []; sel = null; selNet = null;
    ins.innerHTML = "";
    document.getElementById("atlas-n-mis").textContent = "";
    document.getElementById("atlas-n-unv").textContent = "";
    document.getElementById("atlas-p-count").textContent = "";
  }

  // ---------------------------------------------------------------- build
  function build(g) {
    clearMap();
    G = g;
    VOC = g.vocab;
    LAYERS = VOC.layers;
    NL = LAYERS.length;
    var lix = {};
    LAYERS.forEach(function (l, i) { lix[l.id] = i; });
    var lay = g.layout;
    STUB = lay.stub; FONT = lay.font; sceneW = lay.scene_w; bands = lay.bands || [];
    depth = LAYERS.map(function (l) { return lay.depth[l.id] || 110; });
    off = [];
    var acc = 0;
    for (var L = 0; L < NL; L++) { off[L] = acc; acc += depth[L] + GAP; }
    // 3D: each plane's height so that tilted planes stack without overlapping
    Zs = [];
    Zs[NL - 1] = 0;
    for (L = NL - 2; L >= 0; L--) {
      Zs[L] = Zs[L + 1] + ((depth[L] + depth[L + 1]) / 2 * Math.sin(TILT_E) + 80) / Math.cos(TILT_E);
    }
    nets = g.nets;
    nodes = g.nodes.map(function (n) { var m = Object.assign({}, n); m.L = lix[n.layer]; return m; });
    nodes.forEach(function (n) { byId[n.id] = n; });
    edges = g.edges.filter(function (e) { return byId[e.a] && byId[e.b]; });
    adj = {};
    edges.forEach(function (e, i) {
      (adj[e.a] = adj[e.a] || []).push({ to: e.b, i: i });
      (adj[e.b] = adj[e.b] || []).push({ to: e.a, i: i });
    });
    pinsOfNet = {};
    hubParts = {};
    nodes.forEach(function (n) {
      if (n.kind !== "part") return;
      n.sym.pins.forEach(function (p, k) { if (p.net) (pinsOfNet[p.net] = pinsOfNet[p.net] || []).push({ part: n.id, k: k }); });
    });
    edges.forEach(function (e) { if (e.kind === "mate") { hubParts[e.a] = 1; hubParts[e.b] = 1; } });
    problems = g.problems || [];
    problemOf = {};
    problems.forEach(function (p) { p.nodes.forEach(function (id) { (problemOf[id] = problemOf[id] || []).push(p); }); });
    problemIx = -1;

    root = el("g", { "class": "at-root" }, svg);
    layerEls = []; codeNodes = []; billboards = [];
    for (L = NL - 1; L >= 0; L--) {
      var gP = el("g", { "data-layer": L }, root);
      var gE = el("g", {}, root), gN = el("g", {}, root);
      var poly = el("polygon", { "class": "at-plane" }, gP);
      var regs = g.clusters.map(function (c, i) { return el("polygon", { "class": "at-rgn " + (i % 2 ? "b" : "a") }, gP); });
      var lbl = el("text", { "class": "at-layer-lbl" }, gP);
      lbl.textContent = LAYERS[L].name;
      layerEls[L] = { plane: gP, e: gE, n: gN, poly: poly, regs: regs, lbl: lbl };
    }
    var hwL = lix.hw;
    bandEls = bands.map(function (b) {
      var t = el("text", { "class": "at-band-lbl" }, layerEls[hwL].plane);
      t.textContent = b.name;
      var line = el("line", { "class": "at-band-rule" }, layerEls[hwL].plane);
      return { b: b, t: t, line: line };
    });
    clusterEls = g.clusters.map(function (c) {
      var gg = el("g", { "class": "at-cluster" }, root);
      var t = el("text", { "class": "at-cluster-lbl", "text-anchor": "middle" }, gg); t.textContent = c.label;
      var s = el("text", { "class": "at-cluster-sub", "text-anchor": "middle", y: 13 }, gg); s.textContent = c.size;
      el("title", {}, gg).textContent = c.label;
      return { g: gg, c: c, t: t };
    });
    edges.forEach(function (e, i) {
      var a = byId[e.a], b = byId[e.b];
      var same = a.L === b.L;
      var gE = layerEls[same ? a.L : Math.max(a.L, b.L)].e;
      e.el = el("line", { "class": "at-e k-" + e.kind + (e.status !== "ok" ? " " + e.status : ""), "data-e": i }, gE);
    });
    wireG = el("g", { "class": "at-wires" }, layerEls[lix.hw].n);
    nodes.forEach(function (n) { n.el = n.kind === "part" ? buildPart(n) : buildBox(n); });
    pulse = el("circle", { "class": "at-pulse", r: 10 }, root);

    document.getElementById("atlas-n-mis").textContent = g.counts.mismatches + " " + VOC.status.mis;
    document.getElementById("atlas-n-unv").textContent = g.counts.unverified + " " + VOC.status.unv;
    document.getElementById("atlas-p-count").textContent = "–/" + problems.length;
    buildProblemPop();
    buildChips();
    buildLegend();
    buildSearch();
    project();
    fit(true);
    renderInspector();
  }

  // A code-layer node, an external system, a peripheral, an MCU pin: a box (a dot for a pin)
  // that stays readable at every zoom (a billboard, counter-scaled).
  function buildBox(n) {
    var g = el("g", { "class": "at-n k-" + n.kind + " st-" + n.status + (n.zephyr ? " zephyr" : "") + (n.built === false ? " unbuilt" : ""), "data-id": n.id }, layerEls[n.L].n);
    el("title", {}, g).textContent = n.label + (n.sub ? " — " + n.sub : "");
    var inner = el("g", {}, g);
    if (n.kind === "pin") {
      el("circle", { "class": "at-shape", r: 4 }, inner);
      var t = el("text", { "class": "at-lbl at-pin-lbl", y: -8, "text-anchor": "middle" }, inner);
      t.textContent = n.label;
      n.bw = 10; n.bh = 10;
    } else {
      var w = Math.max(56, n.label.length * 6.3 + 20), h = n.sub ? 32 : 22;
      var rx = n.kind === "ext" ? 11 : n.kind === "periph" ? 3 : 6;
      el("rect", { "class": "at-shape", x: -w / 2, y: -h / 2, width: w, height: h, rx: rx }, inner);
      var lt = el("text", { "class": "at-lbl", "text-anchor": "middle", y: n.sub ? -2 : 4 }, inner);
      lt.textContent = n.label;
      if (n.sub) {
        var st = el("text", { "class": "at-sub", "text-anchor": "middle", y: 11 }, inner);
        st.textContent = String(n.sub).length > 34 ? String(n.sub).slice(0, 33) + "…" : n.sub;
      }
      if (n.kind === "module" && n.functions && n.functions.length) {
        var pub = n.functions.filter(function (f) { return f.public; }).slice(0, 14);
        if (pub.length) {
          var fg = el("g", { "class": "at-fns", transform: "translate(" + (-w / 2) + "," + (h / 2 + 4) + ")" }, inner);
          var fw = Math.max(w, Math.max.apply(null, pub.map(function (f) { return f.name.length; })) * 5.6 + 16);
          el("rect", { x: 0, y: 0, width: fw, height: pub.length * 12 + 6, rx: 4 }, fg);
          pub.forEach(function (f, i) { var ft = el("text", { x: 6, y: 12 + i * 12 }, fg); ft.textContent = f.name + "()"; });
        }
      }
      n.bw = w; n.bh = h;
    }
    var mk = el("g", { "class": "at-mark", transform: "translate(" + (n.bw / 2) + "," + (-n.bh / 2) + ")" }, inner);
    el("circle", { r: 6.5 }, mk);
    var mt = el("text", { "class": "at-mark-t", "text-anchor": "middle", y: 3.2 }, mk);
    mt.textContent = n.status === "mis" ? "!" : "?";
    n.inner = inner;
    codeNodes.push(n);
    return g;
  }

  // ---------------------------------------------------------------- symbols
  // Bodies in the part's own coordinates, centred on (0, 0). Two-pin parts stand upright
  // (pins at the top and bottom), so every body below spans y = -10..10.
  var BODY = {
    resistor: "M0,-10 L3,-8.3 L-3,-5 L3,-1.7 L-3,1.7 L3,5 L-3,8.3 L0,10",
    capacitor: "M0,-10 V-2 M-6,-2 H6 M-6,2 H6 M0,2 V10",
    inductor: "M0,-10 a2.5,2.5 0 0 1 0,5 a2.5,2.5 0 0 1 0,5 a2.5,2.5 0 0 1 0,5 a2.5,2.5 0 0 1 0,5",
    ferrite: "M0,-10 V-6 M0,6 V10 M-3,-6 H3 V6 H-3 Z",
    diode: "M0,-10 V-4 M-5,-4 H5 L0,4 Z M-5,4 H5 M0,4 V10",
    led: "M0,-10 V-4 M-5,-4 H5 L0,4 Z M-5,4 H5 M0,4 V10 M6,-3 l4,-4 m-2.5,0 h2.5 v2.5 M6,1 l4,-4 m-2.5,0 h2.5 v2.5",
    tvs: "M0,-10 V-4 M-5,-4 H5 L0,4 Z M-7,6 L-5,4 H5 L7,2 M0,4 V10",
    fuse: "M0,-10 V10 M-3,-7 H3 V7 H-3 Z",
    crystal: "M0,-10 V-6 M-5,-6 H5 M-4,-3 H4 V3 H-4 Z M-5,6 H5 M0,6 V10",
    "switch": "M0,-10 V-5 M0,5 V10 M0,5 L-5,-4",
    nettie: "M0,-10 V10 M-2,-1 H2 M-2,1 H2",
    testpoint: "M-4,0 a4,4 0 1 0 8,0 a4,4 0 1 0 -8,0"
  };
  var FILLED = { diode: "M-5,-4 H5 L0,4 Z", led: "M-5,-4 H5 L0,4 Z", tvs: "M-5,-4 H5 L0,4 Z", ferrite: "M-3,-6 H3 V6 H-3 Z" };

  function stubPath(p) {
    var d = STUB;
    var ix = p.s === "l" ? p.x + d : p.s === "r" ? p.x - d : p.x;
    var iy = p.s === "t" ? p.y + d : p.s === "b" ? p.y - d : p.y;
    return "M" + fx(p.x) + "," + fx(p.y) + "L" + fx(ix) + "," + fx(iy);
  }

  function netText(netId) {
    var n = nets[netId];
    return n ? n.name : "";
  }

  function buildPart(n) {
    var s = n.sym, k = n.sym_kind, upright = s.w <= 12;
    var g = el("g", { "class": "at-n at-part k-part sym-" + k + " st-" + n.status + (n.dni ? " dni" : "") + (n.mcu ? " mcu" : ""), "data-id": n.id }, layerEls[n.L].n);
    var body;
    if (BODY[k] && !(k === "testpoint" && !s.pins.length)) body = BODY[k];
    else if (k === "transistor") body = "M-10,0 a10,10 0 1 0 20,0 a10,10 0 1 0 -20,0 M-10,0 H-5 M-5,-5 V5 M-2,-6 V6 M-2,-4 H0 V-10 M-2,4 H0 V10";
    else body = "M" + fx(-s.w / 2) + "," + fx(-s.h / 2) + "h" + fx(s.w) + "v" + fx(s.h) + "h" + fx(-s.w) + "Z";
    el("path", { "class": "at-sym" + (k === "ic" || k === "connector" ? " box" : ""), d: body }, g);
    if (FILLED[k]) el("path", { "class": "at-sym-fill", d: FILLED[k] }, g);
    el("path", { "class": "at-stub", d: s.pins.map(stubPath).join("") }, g);
    var labels = el("g", { "class": "at-near" }, g);
    // the part's own label: designator, then value or part number
    var lines = (s.lab && s.lab.t ? s.lab.t : n.label).split("\n");
    lines.forEach(function (line, i) {
      var t = el("text", { "class": i ? "at-ref2" : "at-ref", x: s.lab.x, y: fx(s.lab.y + i * FONT * 1.2), "text-anchor": s.lab.a }, labels);
      t.textContent = line;
    });
    var box = k === "ic" || k === "connector" || (k === "transistor" && !upright && s.w > 30);
    s.pins.forEach(function (p, i) {
      // inside a box: the pin's name (SDA, not its number); a connector or a nameless pin: its number
      if (box && !BODY[k]) {
        var inner = k === "connector" || !p.nm ? p.n : p.nm;
        var it;
        if (p.s === "l") it = el("text", { "class": "at-pn", x: fx(-s.w / 2 + 2), y: fx(p.y + FONT * 0.35) }, labels);
        else if (p.s === "r") it = el("text", { "class": "at-pn", x: fx(s.w / 2 - 2), y: fx(p.y + FONT * 0.35), "text-anchor": "end" }, labels);
        else if (p.s === "t") it = el("text", { "class": "at-pn", transform: "translate(" + fx(p.x + FONT * 0.35) + "," + fx(-s.h / 2 + 2) + ") rotate(-90)", "text-anchor": "end" }, labels);
        else it = el("text", { "class": "at-pn", transform: "translate(" + fx(p.x + FONT * 0.35) + "," + fx(s.h / 2 - 2) + ") rotate(-90)" }, labels);
        it.textContent = inner;
      }
      // outside, at the stub's tip: the net, and the firmware's name for it where there is one
      if (!p.net) return;
      var net = nets[p.net] || {};
      var attrs = { "class": "at-nl" + (net.rail ? " rail" : "") + (net.fw ? " fw" : ""), "data-net": p.net, "data-pin": i };
      var t2;
      if (p.s === "l") { attrs.x = fx(p.x - 2); attrs.y = fx(p.y + FONT * 0.35); attrs["text-anchor"] = "end"; }
      else if (p.s === "r") { attrs.x = fx(p.x + 2); attrs.y = fx(p.y + FONT * 0.35); }
      else if (upright) {
        attrs.x = 0; attrs["text-anchor"] = "middle";
        attrs.y = fx(p.s === "t" ? p.y - 2 : p.y + FONT + 0.5);
      } else if (p.s === "t") { attrs.transform = "translate(" + fx(p.x + FONT * 0.35) + "," + fx(p.y - 2) + ") rotate(-90)"; }
      else { attrs.transform = "translate(" + fx(p.x + FONT * 0.35) + "," + fx(p.y + 2) + ") rotate(-90)"; attrs["text-anchor"] = "end"; }
      t2 = el("text", attrs, labels);
      var a = el("tspan", {}, t2); a.textContent = net.name || p.net;
      if (net.fw) { var b = el("tspan", { "class": "at-fw" }, t2); b.textContent = " · " + net.fw.text; }
      el("rect", { "class": "at-pinhit", x: fx(p.x - 3), y: fx(p.y - 3), width: 6, height: 6, "data-net": p.net, "data-pin": i }, g);
    });
    // far from the board, an anchor part keeps a readable name (a billboard)
    if (!upright && k !== "testpoint") {
      var bb = el("g", { "class": "at-far" }, g);
      var ft = el("text", { "class": "at-far-lbl", "text-anchor": "middle" }, bb);
      ft.textContent = n.ref + (n.mpn && (k === "ic" || k === "connector") ? " · " + n.mpn : "");
      n.bb = bb;
      billboards.push(n);
    }
    var mk = el("g", { "class": "at-mark", transform: "translate(" + fx(s.w / 2) + "," + fx(-s.h / 2) + ")" }, g);
    el("circle", { r: 4 }, mk);
    var mt = el("text", { "class": "at-mark-t sm", "text-anchor": "middle", y: 2 }, mk);
    mt.textContent = n.status === "mis" ? "!" : "?";
    return g;
  }

  // ---------------------------------------------------------------- projection
  // Flat (T = 0): each layer is a horizontal band. 3D (T = 1): the planes are seen from
  // above and in front, stacked, turned slightly. Nodes are billboards: they stay upright.
  function proj(x, y, L) {
    var e = lerp(Math.PI / 2, TILT_E, T);
    var psi = lerp(0, -9 * Math.PI / 180, T);
    var dx = x - sceneW / 2, dy = y - depth[L] / 2;
    var xr = dx * Math.cos(psi) - dy * Math.sin(psi);
    var yr = dx * Math.sin(psi) + dy * Math.cos(psi);
    return [xr, yr * Math.sin(e) - Zs[L] * T * Math.cos(e) + lerp(off[L] + depth[L] / 2, 0, T)];
  }
  function pts(list) { return list.map(function (p) { return fx(p[0]) + "," + fx(p[1]); }).join(" "); }

  function project() {
    if (!G) return;
    for (var L = 0; L < NL; L++) {
      var le = layerEls[L];
      le.poly.setAttribute("points", pts([proj(-GUT, -10, L), proj(sceneW + 20, -10, L), proj(sceneW + 20, depth[L] + 10, L), proj(-GUT, depth[L] + 10, L)]));
      G.clusters.forEach(function (c, i) {
        le.regs[i].setAttribute("points", pts([proj(c.x, 0, L), proj(c.x + c.w, 0, L), proj(c.x + c.w, depth[L], L), proj(c.x, depth[L], L)]));
      });
      var lp = proj(-GUT + 10, 16, L);
      le.lblAt = lp;
    }
    var hwL = LAYERS.map(function (l) { return l.id; }).indexOf("hw");
    bandEls.forEach(function (be) {
      be.at = proj(-GUT + 10, be.b.y0 + 40, hwL);
      var a = proj(-GUT, be.b.y0, hwL), b = proj(sceneW + 20, be.b.y0, hwL);
      be.line.setAttribute("x1", fx(a[0])); be.line.setAttribute("y1", fx(a[1]));
      be.line.setAttribute("x2", fx(b[0])); be.line.setAttribute("y2", fx(b[1]));
      be.line.style.display = be.b.y0 > 1 ? "" : "none";
    });
    clusterEls.forEach(function (ce) { ce.at = proj(ce.c.x + ce.c.w / 2, -10, 0); });
    nodes.forEach(function (n) {
      var p = proj(n.x, n.y, n.L);
      n.px = p[0]; n.py = p[1];
      if (n.kind === "part") n.el.setAttribute("transform", "translate(" + fx(n.px) + "," + fx(n.py) + ")");
    });
    edges.forEach(function (e) {
      var a = byId[e.a], b = byId[e.b];
      e.el.setAttribute("x1", fx(a.px)); e.el.setAttribute("y1", fx(a.py));
      e.el.setAttribute("x2", fx(b.px)); e.el.setAttribute("y2", fx(b.py));
    });
    drawWires();
    lastZ = -1;
    render();
  }

  // ---------------------------------------------------------------- camera
  function render() {
    if (!G) return;
    var r = wrap.getBoundingClientRect(), z = cam.z;
    root.setAttribute("transform", "translate(" + fx(r.width / 2) + "," + fx(r.height / 2) + ") scale(" + z.toFixed(5) + ") translate(" + fx(-cam.x) + "," + fx(-cam.y) + ")");
    if (z !== lastZ) {
      lastZ = z;
      // Level of detail, by the size of one pin pitch on screen.
      var pitch = G.layout.pitch * z;
      svg.classList.toggle("lod-far", pitch < 4);
      svg.classList.toggle("lod-mid", pitch >= 4 && pitch < 11);
      svg.classList.toggle("lod-near", pitch >= 11);
      svg.classList.toggle("lod-deep", z >= 1.6);
      var k = Math.min(1, clamp(z * 1.3, 0.8, 1.35) / z);
      codeNodes.forEach(function (n) {
        n.el.setAttribute("transform", "translate(" + fx(n.px) + "," + fx(n.py) + ")");
        n.inner.setAttribute("transform", "scale(" + k.toFixed(4) + ")");
      });
      var inv = 1 / z;
      // A far label shows only where its part is big enough on screen to be told apart.
      billboards.forEach(function (n) {
        var show = n.sym.w * z >= 18 || (n.mcu && n.sym.w * z >= 6);
        n.bb.style.display = show ? "" : "none";
        if (show) n.bb.setAttribute("transform", "translate(0," + fx(-n.sym.h / 2 - 4) + ") scale(" + inv.toFixed(4) + ")");
      });
      for (var L = 0; L < NL; L++) {
        var le = layerEls[L];
        le.lbl.setAttribute("transform", "translate(" + fx(le.lblAt[0]) + "," + fx(le.lblAt[1]) + ") scale(" + inv.toFixed(4) + ")");
      }
      bandEls.forEach(function (be) { be.t.setAttribute("transform", "translate(" + fx(be.at[0]) + "," + fx(be.at[1]) + ") scale(" + inv.toFixed(4) + ")"); });
      clusterEls.forEach(function (ce) {
        // a column too narrow for its name shows as much of it as fits
        var room = Math.floor((ce.c.w * z - 10) / 7);
        ce.t.textContent = room >= ce.c.label.length ? ce.c.label : ce.c.label.slice(0, Math.max(0, room - 1)) + "…";
        ce.g.style.display = room >= 4 ? "" : "none";
        ce.g.setAttribute("transform", "translate(" + fx(ce.at[0]) + "," + fx(ce.at[1]) + ") scale(" + inv.toFixed(4) + ") translate(0,-26)");
      });
      pulse.setAttribute("r", fx(10 / z));
      document.getElementById("atlas-zoom").textContent = (z * 100 < 10 ? (z * 100).toFixed(1) : Math.round(z * 100)) + "%";
    } else {
      codeNodes.forEach(function (n) { n.el.setAttribute("transform", "translate(" + fx(n.px) + "," + fx(n.py) + ")"); });
    }
    for (var L2 = 0; L2 < NL; L2++) {
      var vis = !hidden[L2];
      layerEls[L2].plane.style.display = vis ? "" : "none";
      layerEls[L2].n.style.display = vis ? "" : "none";
      layerEls[L2].e.style.display = vis ? "" : "none";
    }
    if (sel && byId[sel]) { pulse.setAttribute("cx", fx(byId[sel].px)); pulse.setAttribute("cy", fx(byId[sel].py)); }
  }
  function schedule() {
    if (rafPending) return;
    rafPending = true;
    requestAnimationFrame(function () { rafPending = false; render(); });
  }

  function fit(now) {
    if (!G) return;
    var xs = [], ys = [];
    for (var L = 0; L < NL; L++) {
      if (hidden[L]) continue;
      [[-GUT, -10], [sceneW + 20, -10], [sceneW + 20, depth[L] + 10], [-GUT, depth[L] + 10]].forEach(function (c) {
        var p = proj(c[0], c[1], L); xs.push(p[0]); ys.push(p[1]);
      });
    }
    if (!xs.length) return;
    var r = wrap.getBoundingClientRect();
    var minx = Math.min.apply(null, xs) - 20, maxx = Math.max.apply(null, xs) + 20;
    var miny = Math.min.apply(null, ys) - 50, maxy = Math.max.apply(null, ys) + 30;
    var z = Math.min((r.width || 800) / (maxx - minx), (r.height || 600) / (maxy - miny));
    zMin = z * 0.5;
    var tx = (minx + maxx) / 2, ty = (miny + maxy) / 2;
    if (now) { cam.x = tx; cam.y = ty; cam.z = z; render(); }
    else animateCam(tx, ty, z, 380);
  }
  function animateCam(tx, ty, tz, ms) {
    var f = { x: cam.x, y: cam.y, z: cam.z }, t0 = performance.now();
    (function step(now) {
      var k = Math.min(1, (now - t0) / ms), e = ease(k);
      cam.x = lerp(f.x, tx, e); cam.y = lerp(f.y, ty, e);
      cam.z = Math.exp(lerp(Math.log(f.z), Math.log(tz), e));
      render();
      if (k < 1) requestAnimationFrame(step);
    })(t0);
  }
  function centerOn(id, z) {
    var n = byId[id];
    if (!n) return;
    if (hidden[n.L]) { hidden[n.L] = false; buildChips(); }
    animateCam(n.px, n.py, z || cam.z, 420);
  }
  function setTilt(t) {
    T = t;
    document.getElementById("atlas-tilt").value = Math.round(T * 100);
    project();
  }
  function animateTilt(to) {
    var from = T, t0 = performance.now(), keep = sel ? byId[sel] : null;
    (function step(now) {
      var k = Math.min(1, (now - t0) / 480);
      setTilt(lerp(from, to, ease(k)));
      if (keep) { cam.x = keep.px; cam.y = keep.py; render(); } else fit(true);
      if (k < 1) requestAnimationFrame(step);
    })(t0);
  }

  // ---------------------------------------------------------------- selection and the chain
  // Up and down the stack from one node. A shared hub (a peripheral, an in-tree Zephyr
  // module, the MCU package) is shown but not walked through, or one part would light up
  // everything on its bus; a selected pin still opens its own peripheral.
  function isHub(n) {
    return n.kind === "periph" || n.zephyr || n.mcu;
  }
  function chainOf(id) {
    var on = {}, eon = {}, rootN = byId[id];
    on[id] = 1;
    function walk(dir) {
      var q = [{ id: id, same: 0 }], seen = {};
      seen[id] = 1;
      while (q.length) {
        var cur = q.shift(), cn = byId[cur.id];
        (adj[cur.id] || []).forEach(function (a) {
          var m = byId[a.to], e = edges[a.i];
          var dl = m.L - cn.L;
          if (dir > 0 ? dl < 0 : dl > 0) return;
          if (e.kind === "pad" && !(rootN.kind === "pin" && cur.id === id) && !(rootN.mcu && cur.id === id)) return;
          var same = dl === 0 ? cur.same + 1 : cur.same;
          if (same > 1) return;
          if (dl === 0 && cn.kind === "part" && cur.id !== id) return;
          eon[a.i] = 1; on[m.id] = 1;
          if (seen[m.id]) return;
          seen[m.id] = 1;
          if (isHub(m) && !(cur.id === id && rootN.kind === "pin" && m.kind === "periph")) return;
          if (dl === 0 && m.kind === "part") return;
          q.push({ id: m.id, same: same });
        });
      }
    }
    walk(+1); walk(-1);
    return { on: on, eon: eon };
  }

  // Parts one hop away on the board, through a signal net (rails are not followed).
  function boardNeighbours(id) {
    var n = byId[id], out = {};
    if (!n || n.kind !== "part") return out;
    n.sym.pins.forEach(function (p) {
      if (!p.net || (nets[p.net] && nets[p.net].rail)) return;
      netMembers(p.net).forEach(function (m) { if (m.part !== id) out[m.part] = 1; });
    });
    return out;
  }
  function netMembers(netId) {
    var out = (pinsOfNet[netId] || []).slice();
    ((nets[netId] || {}).mates || []).forEach(function (m) { out = out.concat(pinsOfNet[m] || []); });
    return out;
  }

  function clearMarks() {
    svg.querySelectorAll(".on, .sel, .net-on").forEach(function (x) { x.classList.remove("on", "sel", "net-on"); });
  }

  function select(id) {
    sel = id && byId[id] ? id : null;
    selNet = null; selPin = null;
    clearMarks();
    svg.classList.toggle("has-sel", !!sel);
    if (sel) {
      var ch = chainOf(sel);
      Object.keys(ch.on).forEach(function (k) { byId[k].el.classList.add("on"); });
      Object.keys(ch.eon).forEach(function (i) { edges[i].el.classList.add("on"); });
      Object.keys(boardNeighbours(sel)).forEach(function (k) { byId[k].el.classList.add("on"); });
      byId[sel].el.classList.add("sel");
      byId[sel].el.parentNode.appendChild(byId[sel].el);
    }
    drawWires();
    renderInspector();
    schedule();
  }

  function selectNet(netId, partId, pinIx) {
    if (!nets[netId]) return;
    sel = partId || null; selNet = netId; selPin = pinIx == null ? null : pinIx;
    clearMarks();
    svg.classList.add("has-sel");
    netMembers(netId).forEach(function (m) { byId[m.part].el.classList.add("on"); });
    nodes.forEach(function (n) {
      if ((n.kind === "pin" && n.net === netId)) n.el.classList.add("on");
    });
    svg.querySelectorAll('.at-nl[data-net="' + CSS.escape(netId) + '"]').forEach(function (t) { t.classList.add("net-on"); });
    drawWires();
    renderInspector();
    schedule();
  }

  // A wire leaves a pin from beyond its net label, so it never runs through the text:
  // [tip x, tip y, exit x, exit y].
  function pinPos(partId, k) {
    var n = byId[partId], p = n.sym.pins[k], upright = n.sym.w <= 12;
    var net = nets[p.net] || {};
    var len = ((net.name || "").length + (net.fw ? net.fw.text.length + 3 : 0)) * FONT * 0.6 + 4;
    var x = n.px + p.x, y = n.py + p.y, ex = x, ey = y;
    if (p.s === "l") ex = x - len;
    else if (p.s === "r") ex = x + len;
    else if (upright) ex = x + Math.max(len / 2, 8);
    else ey = p.s === "t" ? y - len : y + len;
    return [x, y, ex, ey, p.s];
  }
  // The label is the net's name on the pin, as on a schematic; the wire joins label ends.
  function wire(a, b, cls) {
    // The vertical run sits outside both pins' labels on the side they leave from, so a
    // wire from a left-side pin never cuts back through its own part.
    var mx = (a[2] + b[2]) / 2;
    if (a[4] === "l" || (a[4] !== "r" && b[4] === "l")) mx = Math.min(a[2], b[2]) - 5;
    else if (a[4] === "r" || b[4] === "r") mx = Math.max(a[2], b[2]) + 5;
    var d = "M" + fx(a[2]) + "," + fx(a[3]) + "L" + fx(mx) + "," + fx(a[3]) + "L" + fx(mx) + "," + fx(b[3]) + "L" + fx(b[2]) + "," + fx(b[3]);
    el("path", { "class": "at-wire " + (cls || ""), d: d }, wireG);
    el("circle", { "class": "at-joint " + (cls || ""), cx: fx(b[2]), cy: fx(b[3]), r: 1.6 }, wireG);
  }

  // Wires are drawn only for what is selected: from the selected part's pins to every other
  // pin on the same signal net, or, for a selected net, from one pin to all the others.
  function drawWires() {
    if (!wireG) return;
    while (wireG.firstChild) wireG.removeChild(wireG.firstChild);
    if (selNet) {
      var mem = netMembers(selNet);
      var from = sel && selPin != null ? [sel, selPin] : (mem[0] ? [mem[0].part, mem[0].k] : null);
      if (!from) return;
      var a = pinPos(from[0], from[1]);
      el("circle", { "class": "at-joint net", cx: fx(a[2]), cy: fx(a[3]), r: 1.6 }, wireG);
      mem.forEach(function (m) {
        if (m.part === from[0] && m.k === from[1]) return;
        if (byId[m.part].board !== byId[from[0]].board) return;
        wire(a, pinPos(m.part, m.k), "net");
      });
      return;
    }
    if (!sel || byId[sel].kind !== "part") return;
    var n = byId[sel];
    // A hub (the MCU, a board-to-board connector) would draw a wire to half the board;
    // its nets are drawn one at a time, from a click on the net's label.
    if (n.mcu || hubParts[sel]) return;
    n.sym.pins.forEach(function (p, k) {
      if (!p.net || (nets[p.net] && nets[p.net].rail)) return;
      var a2 = pinPos(sel, k), any = false;
      (pinsOfNet[p.net] || []).forEach(function (m) {
        if (m.part !== sel && !any) { any = true; el("circle", { "class": "at-joint", cx: fx(a2[2]), cy: fx(a2[3]), r: 1.6 }, wireG); }
        if (m.part === sel) return;
        wire(a2, pinPos(m.part, m.k), (nets[p.net].fw ? "fw" : ""));
      });
    });
  }

  svg.addEventListener("click", function (ev) {
    if (moved) return;
    var hit = ev.target.closest("[data-net]");
    var nd = ev.target.closest(".at-n");
    if (hit && nd && byId[nd.dataset.id] && byId[nd.dataset.id].kind === "part") {
      selectNet(hit.dataset.net, nd.dataset.id, +hit.dataset.pin);
      return;
    }
    if (nd) { select(nd.dataset.id); return; }
    select(null);
  });
  svg.addEventListener("dblclick", function (ev) {
    var nd = ev.target.closest(".at-n");
    if (nd) { select(nd.dataset.id); centerOn(nd.dataset.id, Math.max(cam.z, 2.2)); }
  });

  // ---------------------------------------------------------------- problems
  function buildProblemPop() {
    var pop = document.getElementById("atlas-problem-pop");
    pop.innerHTML = "";
    problems.forEach(function (p, i) {
      var b = document.createElement("button");
      b.type = "button";
      b.innerHTML = '<span class="badge ' + (p.sev === "mis" ? "badge-danger" : "badge-warning") + '">' + esc(p.id) + "</span><span>" + esc(p.title) + "</span>";
      b.onclick = function () { gotoProblem(i); };
      pop.appendChild(b);
    });
  }
  function gotoProblem(ix) {
    if (!problems.length) return;
    problemIx = (ix + problems.length) % problems.length;
    var p = problems[problemIx];
    var first = p.nodes.filter(function (i) { return byId[i]; })[0];
    if (!first) return;
    select(first);
    var n = byId[first];
    centerOn(first, n.kind === "part" ? Math.max(cam.z, 1.4) : Math.max(cam.z, 0.9));
    pulse.classList.remove("go");
    void pulse.getBBox();
    pulse.classList.toggle("unv", p.sev !== "mis");
    pulse.classList.add("go");
    document.getElementById("atlas-p-count").textContent = (problemIx + 1) + "/" + problems.length;
    document.getElementById("atlas-problem-pop").classList.remove("open");
    renderInspector(p);
  }
  document.getElementById("atlas-p-next").onclick = function () { gotoProblem(problemIx + 1); };
  document.getElementById("atlas-p-prev").onclick = function () { gotoProblem(problemIx < 0 ? problems.length - 1 : problemIx - 1); };
  document.getElementById("atlas-p-count").onclick = function () { document.getElementById("atlas-problem-pop").classList.toggle("open"); };

  // ---------------------------------------------------------------- inspector
  function layerName(n) { return LAYERS[n.L] ? LAYERS[n.L].name : ""; }
  function kindName(n) {
    if (n.kind === "part") return (VOC.symbols[n.sym_kind] || VOC.kinds.part);
    return VOC.kinds[n.kind] || n.kind;
  }
  function statusBadge(st) {
    var cls = st === "mis" ? "badge-danger" : st === "unv" ? "badge-warning" : st === "info" ? "badge-neutral" : "badge-success";
    return '<span class="badge ' + cls + '">' + esc(VOC.status[st] || st) + "</span>";
  }
  function linkBtn(id, via, st) {
    var n = byId[id];
    if (!n) return "";
    return '<button type="button" class="at-link ' + (st && st !== "ok" ? st : "") + '" data-go="' + esc(id) + '">' + esc(n.label) +
      (via ? '<span class="via">' + esc(via) + "</span>" : "") + "</button>";
  }
  function netBtn(netId, label) {
    var n = nets[netId];
    if (!n) return esc(label || netId);
    return '<button type="button" class="at-link net' + (n.rail ? " rail" : "") + '" data-netgo="' + esc(netId) + '">' + esc(label || n.name) + "</button>";
  }
  function clusterOf(n) {
    var c = G.clusters.filter(function (c) { return c.id === n.cluster; })[0];
    return c ? c.label : "";
  }

  var citeSeq = 0, citeList = [];
  function citeHTML(c) {
    if (!c) return "";
    var i = citeSeq++;
    citeList[i] = c;
    if (c.t === "code") {
      return '<div class="at-cite" data-cite="' + i + '"><div class="at-cite-head"><span class="at-cite-doc">' + esc(c.path) + (c.line ? ":" + c.line : "") +
        '</span><span class="at-code-state"></span><a class="btn at-vscode" style="display:none">Open in VS Code ↗</a></div><div class="at-code-ex">…</div></div>';
    }
    var doc = (G.docs || {})[c.doc] || {};
    var sch = doc.kind === "schematic";
    var canCard = !sch && (c.card || doc.card);
    var canSec = !sch && !!c.file;
    var canPage = !!(c.p && doc.pdf);
    return '<div class="at-cite" data-cite="' + i + '"><div class="at-cite-head"><span class="at-cite-doc">' + esc(doc.file || c.doc) + (c.p ? " · p." + c.p : "") + "</span>" +
      '<span class="muted at-cite-sec">' + esc(c.sec ? (c.sec + (c.sec_title ? " " + c.sec_title : "")) : "") + "</span>" +
      '<span class="at-tiers"><button type="button" data-tier="card"' + (canCard ? "" : " disabled") + ">Card</button>" +
      '<button type="button" data-tier="sec"' + (canSec ? "" : " disabled") + ">Section</button>" +
      '<button type="button" data-tier="page"' + (canPage ? "" : " disabled") + ">Page</button>" +
      '<button type="button" data-tier="pdf"' + (doc.pdf ? "" : " disabled") + ">PDF</button></span></div>" +
      '<div class="at-tier-body">' + (c.q ? '<span class="mono">' + esc(c.q) + "</span>" : "") + "</div></div>";
  }

  function loadCode(div, c) {
    api("/api/atlas/code?id=" + encodeURIComponent(atlasId) + "&root=" + encodeURIComponent(c.root || "fw") + "&path=" + encodeURIComponent(c.path) + (c.line ? "&line=" + c.line : ""))
      .then(function (r) { return r.json(); }).then(function (x) {
        var st = div.querySelector(".at-code-state");
        st.className = "at-code-state badge " + (x.state === "unchanged" ? "badge-neutral" : x.state === "changed" ? "badge-warning" : "badge-neutral");
        st.textContent = x.state === "unchanged" ? "unchanged since " + G.commit.slice(0, 7) : x.state === "changed" ? "changed since " + G.commit.slice(0, 7) : x.state;
        var a = div.querySelector(".at-vscode");
        a.href = x.link; a.style.display = "";
        div.querySelector(".at-code-ex").innerHTML = x.lines.map(function (l) {
          var ln = '<span class="ln">' + l[0] + "</span>" + esc(l[1]);
          return l[0] === x.line ? '<span class="hl">' + ln + "</span>" : ln;
        }).join("\n");
      }).catch(function (e) { div.querySelector(".at-code-ex").textContent = e.message; });
  }

  function showTier(div, c, tier) {
    div.querySelectorAll(".at-tiers button").forEach(function (b) { b.classList.toggle("on", b.dataset.tier === tier); });
    var body = div.querySelector(".at-tier-body");
    body.className = "at-tier-body";
    if (tier === "card") {
      if (c.card) { body.className = "at-tier-body md"; body.textContent = c.card; return; }
      body.textContent = "…";
      api("/api/atlas/doc/" + encodeURIComponent(c.doc) + "/card.md").then(function (r) { return r.text(); })
        .then(function (t) { body.className = "at-tier-body md"; body.textContent = t; })
        .catch(function (e) { body.textContent = e.message; });
    } else if (tier === "sec") {
      body.textContent = "…";
      api("/api/atlas/doc/" + encodeURIComponent(c.doc) + "/" + c.file).then(function (r) { return r.text(); })
        .then(function (t) { body.className = "at-tier-body md"; body.textContent = t; })
        .catch(function (e) { body.textContent = e.message; });
    } else if (tier === "page") {
      var src = "/api/atlas/" + encodeURIComponent(atlasId) + "/page/" + encodeURIComponent(c.doc) + "/" + c.p;
      body.innerHTML = '<div class="at-page"><img alt="" src="' + src + '"><svg class="at-page-box"></svg></div>';
      var img = body.querySelector("img"), ov = body.querySelector("svg");
      img.onload = function () {
        ov.setAttribute("viewBox", "0 0 " + img.naturalWidth + " " + img.naturalHeight);
        if (c.box) {
          var k = pageDpi / 72, b = c.box;
          el("rect", { x: b[0] * k, y: b[1] * k, width: (b[2] - b[0]) * k, height: (b[3] - b[1]) * k, rx: 3 }, ov);
        }
      };
      img.onerror = function () {
        api(src).catch(function (e) { body.textContent = e.message; });
      };
    } else if (tier === "pdf") {
      window.open("/api/atlas/" + encodeURIComponent(atlasId) + "/pdf/" + encodeURIComponent(c.doc) + (c.p ? "#page=" + c.p : ""), "_blank");
    }
  }

  function wireCites() {
    ins.querySelectorAll(".at-cite[data-cite]").forEach(function (div) {
      var c = citeList[+div.dataset.cite];
      if (c && c.t === "code") loadCode(div, c);
    });
  }

  function problemHTML(p, open) {
    var h = '<div class="at-problem ' + p.sev + '"><div class="at-problem-title">' + esc(p.id) + " · " + esc(p.title) + "</div>";
    if (p.settle) h += '<div class="muted at-settle">' + esc(p.settle) + "</div>";
    if (open !== false) {
      h += '<table class="at-src-table">';
      p.rows.forEach(function (r) {
        h += "<tr><td>" + esc(r.src) + (r.verdict ? '<div class="muted">' + esc(r.verdict) + "</div>" : "") + '</td><td><div class="says">' + esc(r.says) + "</div>" + citeHTML(r.cite) + "</td></tr>";
      });
      h += "</table>";
    }
    return h + "</div>";
  }

  function renderInspector(focus) {
    citeSeq = 0; citeList = [];
    if (!G) { ins.innerHTML = ""; return; }
    var h = "";
    if (focus) h += '<div class="at-sec">' + problemHTML(focus) + "</div>";
    if (selNet) h += netInspector(selNet);
    else if (sel) h += nodeInspector(byId[sel], focus);
    else h += overview();
    ins.innerHTML = h;
    wireCites();
  }

  function overview() {
    var h = '<div class="at-kicker">' + esc(G.target || "") + " · " + esc(G.board || "") + '</div><div class="at-title mono">' + esc(G.atlas) + "</div>";
    h += '<div class="at-badges"><span class="badge badge-danger">' + G.counts.mismatches + " " + esc(VOC.status.mis) + '</span><span class="badge badge-warning">' +
      G.counts.unverified + " " + esc(VOC.status.unv) + '</span><span class="badge badge-neutral">' + G.counts.part + " parts · " + G.counts.nets + " nets</span></div>";
    h += '<div class="at-kv"><span class="k">commit</span><span class="mono">' + esc((G.commit || "").slice(0, 12)) + '</span><span class="k">app</span><span class="mono">' + esc(G.app || "") +
      '</span><span class="k">built</span><span class="mono">' + esc(G.created || "") + "</span></div>";
    if (G.summary) h += '<div class="at-sec muted">' + esc(G.summary) + "</div>";
    h += '<div class="at-sec"><h4>Checks</h4><div class="at-src-list">';
    (G.checks || []).forEach(function (c) {
      var ok = c.candidates === 0;
      h += '<div class="at-src-row"><span>' + esc(c.check) + '</span><span class="badge ' + (ok ? "badge-success" : "badge-warning") + '">' + c.passed + "/" + c.ran + "</span></div>";
    });
    h += "</div></div>";
    var info = G.info || {};
    if (Object.keys(info).length) {
      h += '<div class="at-sec"><h4>' + esc(VOC.status.info) + '</h4><div class="at-src-list">';
      Object.keys(info).forEach(function (cls) {
        h += '<details class="at-info"><summary><span>' + esc(cls) + '</span><span class="badge badge-neutral">' + info[cls].length + "</span></summary>";
        info[cls].forEach(function (p) {
          h += '<div class="at-info-row">' + p.nodes.slice(0, 3).map(function (i) { return linkBtn(i); }).join("") + " " + esc(p.title) + "</div>";
        });
        h += "</details>";
      });
      h += "</div></div>";
    }
    h += '<div class="at-sec"><h4>Problems</h4>';
    problems.forEach(function (p, i) {
      h += '<button type="button" class="at-link ' + p.sev + ' at-prob" data-prob="' + i + '"><b>' + esc(p.id) + "</b> · " + esc(p.title) + "</button>";
    });
    h += '</div><div class="at-sec"><h4>Sources</h4><div class="at-src-list">';
    Object.keys(G.docs || {}).forEach(function (d) {
      var x = G.docs[d];
      h += '<div class="at-src-row"><span class="mono">' + esc(x.file || d) + '</span><span class="muted">' + esc(d) + (x.pages ? " · " + x.pages + " pp" : "") + (x.pdf ? "" : " · PDF missing") + "</span></div>";
    });
    if (G.code_note) h += '<div class="at-src-row"><span class="mono">ELF</span><span class="muted">' + esc(G.code_note) + "</span></div>";
    return h + "</div></div>";
  }

  function chainRows(n) {
    var ch = chainOf(n.id), rows = {};
    Object.keys(ch.eon).forEach(function (i) {
      var e = edges[i];
      [e.a, e.b].forEach(function (id) {
        if (id === n.id) return;
        var m = byId[id];
        rows[m.L] = rows[m.L] || {};
        var via = e.label || "";
        var pri = (e.a === n.id || e.b === n.id ? 4 : 0) + (byId[e.a].L !== byId[e.b].L ? 2 : 0) + (via ? 1 : 0);
        if (!rows[m.L][id] || pri > rows[m.L][id].pri) rows[m.L][id] = { via: via, pri: pri, st: e.status !== "ok" ? e.status : m.status };
      });
    });
    var h = '<div class="at-sec"><h4>Up and down the stack</h4><div class="at-chain">';
    for (var L = 0; L < NL; L++) {
      if (L === n.L) {
        var peers = rows[L] ? Object.keys(rows[L]) : [];
        h += '<div class="at-chain-row"><span class="ly">' + esc(LAYERS[L].name) + '</span><span class="its"><b>' + esc(n.label) + "</b>" +
          peers.map(function (i) { return linkBtn(i, rows[L][i].via, rows[L][i].st); }).join("") + "</span></div>";
        continue;
      }
      if (!rows[L]) continue;
      h += '<div class="at-chain-row"><span class="ly">' + esc(LAYERS[L].name) + '</span><span class="its">' +
        Object.keys(rows[L]).map(function (i) { return linkBtn(i, rows[L][i].via, rows[L][i].st); }).join("") + "</span></div>";
    }
    return h + "</div></div>";
  }

  function nodeInspector(n, focus) {
    var h = '<div class="at-kicker">' + esc(kindName(n)) + " · " + esc(layerName(n)) + '</div><div class="at-title' + (n.kind === "part" || n.kind === "pin" ? " mono" : "") + '">' + esc(n.label) + "</div>";
    h += '<div class="at-badges">' + statusBadge(n.status) + (n.cluster ? '<span class="badge badge-neutral">' + esc(clusterOf(n)) + "</span>" : "") +
      (n.zephyr ? '<span class="badge badge-neutral">zephyr</span>' : "") + (n.built === false ? '<span class="badge badge-warning">' + esc(n.sub) + "</span>" : "") +
      (n.dni ? '<span class="badge badge-warning">DNI</span>' : "") + "</div>";
    if (n.path) h += '<div class="at-path">' + esc(n.path) + "</div>";
    var kv = [];
    if (n.kind === "part") {
      if (n.mpn) kv.push(["MPN", n.mpn]);
      if (n.value) kv.push(["value", n.value]);
      if (n.desc) kv.push(["BOM", n.desc]);
      if (n.sheet) kv.push(["sheet", n.sheet + (n.pages && n.pages.length ? " · p." + n.pages.join(",") : "")]);
      if (n.of) kv.push(["sits by", null, linkBtn(n.of)]);
    } else if (n.kind === "periph") {
      if (n.node) kv.push(["DT node", n.node]);
      if (n.compat) kv.push(["compatible", n.compat]);
    } else if (n.kind === "pin") {
      kv.push(["package pin", n.num]);
      if (n.net) kv.push(["net", null, netBtn(n.net)]);
      if (n.io) kv.push(["I/O", n.io]);
    } else if (n.kind === "module") {
      kv.push(["functions", n.n_functions + " (" + n.n_public + " public)"]);
    }
    if (kv.length) {
      h += '<div class="at-kv">' + kv.map(function (r) { return '<span class="k">' + esc(r[0]) + '</span><span class="mono">' + (r[2] || esc(r[1])) + "</span>"; }).join("") + "</div>";
    }
    (problemOf[n.id] || []).forEach(function (p) { if (p !== focus) h += '<div class="at-sec">' + problemHTML(p) + "</div>"; });
    var infos = [];
    Object.keys(G.info || {}).forEach(function (cls) { G.info[cls].forEach(function (p) { if (p.nodes.indexOf(n.id) >= 0) infos.push(p); }); });
    if (infos.length) {
      h += '<div class="at-sec"><h4>' + esc(VOC.status.info) + "</h4>";
      infos.forEach(function (p) { h += '<div class="at-info-row"><b>' + esc(p.cls) + "</b> " + esc(p.title) + "</div>"; });
      h += "</div>";
    }
    if (n.kind === "part") h += partPins(n) + partDocs(n);
    if (n.kind === "pin" && n.uses && n.uses.length) {
      h += '<div class="at-sec"><h4>Devicetree</h4>';
      n.uses.forEach(function (u) {
        h += '<div class="at-fact"><div class="at-fact-head mono">' + esc(u.label || u.node) + " · " + esc(u.prop) + (u.func ? " · " + esc(u.func) : "") + (u.flags ? " · " + esc(u.flags) : "") + "</div>" +
          (u.loc && /^fw:/.test(u.loc) ? citeHTML(codeCite(u.loc)) : "") + "</div>";
      });
      h += "</div>";
    }
    h += chainRows(n);
    if (n.kind === "part") {
      var nb = Object.keys(boardNeighbours(n.id));
      if (nb.length) h += '<div class="at-sec"><h4>On the board, one net away</h4><div class="at-chain-row"><span class="its">' + nb.sort().map(function (i) { return linkBtn(i); }).join("") + "</span></div></div>";
    }
    if (n.kind === "module" && n.functions && n.functions.length) {
      h += '<div class="at-sec"><h4>Functions</h4><div class="at-fn-list">';
      n.functions.forEach(function (f, i) {
        h += '<button type="button" class="at-fn' + (f.public ? "" : " static") + '" data-fn="' + i + '">' + esc(f.name) + "()</button>";
      });
      h += '</div><div id="atlas-fn-cite"></div></div>';
    }
    return h;
  }

  function codeCite(loc) {
    var m = /^(fw|zephyr):(.+?):(\d+)$/.exec(loc || "");
    return m ? { t: "code", root: m[1], path: m[2], line: +m[3] } : null;
  }

  function partPins(n) {
    var h = '<div class="at-sec"><h4>Pins</h4><table class="at-pins"><tr><th>pin</th><th>name</th><th>net</th></tr>';
    n.sym.pins.forEach(function (p) {
      var net = nets[p.net] || null;
      h += '<tr><td class="mono">' + esc(p.n) + '</td><td class="mono">' + esc(p.nm || "") + "</td><td>" + (p.net ? netBtn(p.net) : '<span class="muted">—</span>') +
        (net && net.fw ? '<div class="at-fw-line">' + esc(net.fw.text) + "</div>" : "") + "</td></tr>";
    });
    return h + "</table></div>";
  }

  function partDocs(n) {
    var h = "";
    (n.docs || []).forEach(function (d) {
      h += '<div class="at-sec"><h4>' + esc(d) + "</h4>" + citeHTML({ t: "doc", doc: d, p: null, file: "toc.md" }) + "</div>";
    });
    if (n.pages && n.pages.length) {
      h += '<div class="at-sec"><h4>Schematic</h4>' + citeHTML({ t: "doc", doc: "sch:" + n.board, p: n.pages[0], sec: n.sheet, box: n.box || null }) + "</div>";
    }
    return h;
  }

  function netInspector(netId) {
    var n = nets[netId];
    var h = '<div class="at-kicker">net · board ' + esc(n.board) + '</div><div class="at-title mono">' + esc(n.name) + "</div>";
    h += '<div class="at-badges">' + (n.rail ? '<span class="badge badge-neutral">' + (n.ground ? "ground" : "rail") + "</span>" : "") +
      (n.fw ? '<span class="badge badge-accent">' + esc(n.fw.text) + "</span>" : "") + "</div>";
    if (n.fw) h += '<div class="at-kv"><span class="k">MCU pin</span><span>' + n.fw.port.map(function (p) { return linkBtn("pin:" + p); }).join("") + '</span><span class="k">firmware</span><span class="mono">' +
      esc(n.fw.uses.join(", ")) + '</span><span class="k">DT node</span><span class="mono">' + esc(n.fw.dt.join(", ")) + "</span></div>";
    if (n.mates && n.mates.length) h += '<div class="at-sec"><h4>Across the connector</h4>' + n.mates.map(function (m) { return netBtn(m, m); }).join(" ") + "</div>";
    var mem = netMembers(netId);
    h += '<div class="at-sec"><h4>Pins on this net · ' + mem.length + '</h4><div class="at-chain-row"><span class="its">';
    mem.forEach(function (m) {
      var p = byId[m.part].sym.pins[m.k];
      h += linkBtn(m.part, (p.nm || p.n));
    });
    return h + "</span></div></div>";
  }

  ins.addEventListener("click", function (ev) {
    var t = ev.target;
    var go = t.closest("[data-go]");
    if (go) { select(go.dataset.go); centerOn(go.dataset.go); return; }
    var ng = t.closest("[data-netgo]");
    if (ng) {
      var mem = netMembers(ng.dataset.netgo);
      selectNet(ng.dataset.netgo, mem[0] && mem[0].part, mem[0] && mem[0].k);
      if (mem[0]) centerOn(mem[0].part, Math.max(cam.z, 1.2));
      return;
    }
    var pr = t.closest("[data-prob]");
    if (pr) { gotoProblem(+pr.dataset.prob); return; }
    var tier = t.closest("[data-tier]");
    if (tier && !tier.disabled) {
      var div = tier.closest(".at-cite");
      showTier(div, citeList[+div.dataset.cite], tier.dataset.tier);
      return;
    }
    var fn = t.closest("[data-fn]");
    if (fn && sel) {
      var f = byId[sel].functions[+fn.dataset.fn];
      var box = document.getElementById("atlas-fn-cite");
      citeSeq = citeList.length;
      box.innerHTML = citeHTML({ t: "code", root: byId[sel].root || "fw", path: f.path, line: f.line });
      loadCode(box.querySelector(".at-cite"), citeList[citeList.length - 1]);
    }
  });

  // ---------------------------------------------------------------- controls
  var drag = null, moved = false;
  svg.addEventListener("pointerdown", function (e) { drag = { x: e.clientX, y: e.clientY, cx: cam.x, cy: cam.y }; moved = false; });
  window.addEventListener("pointermove", function (e) {
    if (!drag) return;
    var dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) { moved = true; svg.classList.add("dragging"); }
    cam.x = drag.cx - dx / cam.z; cam.y = drag.cy - dy / cam.z;
    schedule();
  });
  window.addEventListener("pointerup", function () { drag = null; svg.classList.remove("dragging"); setTimeout(function () { moved = false; }, 0); });
  svg.addEventListener("wheel", function (e) {
    e.preventDefault();
    var r = wrap.getBoundingClientRect();
    var mx = e.clientX - r.left - r.width / 2, my = e.clientY - r.top - r.height / 2;
    var wx = cam.x + mx / cam.z, wy = cam.y + my / cam.z;
    cam.z = clamp(cam.z * Math.exp(-e.deltaY * 0.0015), zMin, zMax);
    cam.x = wx - mx / cam.z; cam.y = wy - my / cam.z;
    schedule();
  }, { passive: false });
  document.getElementById("atlas-tilt").addEventListener("input", function (e) {
    setTilt(e.target.value / 100);
    if (sel) { cam.x = byId[sel].px; cam.y = byId[sel].py; render(); } else fit(true);
  });
  document.getElementById("atlas-fit").onclick = function () { fit(); };

  function buildChips() {
    var box = document.getElementById("atlas-layer-chips");
    box.innerHTML = "";
    LAYERS.forEach(function (ly, L) {
      var b = document.createElement("button");
      b.type = "button";
      b.className = "at-chip" + (hidden[L] ? " off" : "");
      b.textContent = ly.name;
      b.onclick = function () { hidden[L] = !hidden[L]; buildChips(); schedule(); };
      box.appendChild(b);
    });
  }
  function buildLegend() {
    var lg = document.getElementById("atlas-legend");
    lg.innerHTML = '<span><svg width="14" height="10"><line x1="0" y1="5" x2="14" y2="5" class="lg-mis"/></svg>' + esc(VOC.status.mis) + "</span>" +
      '<span><svg width="14" height="10"><line x1="0" y1="5" x2="14" y2="5" class="lg-unv"/></svg>' + esc(VOC.status.unv) + "</span>" +
      '<span><svg width="14" height="10"><line x1="0" y1="5" x2="14" y2="5" class="lg-fw"/></svg>firmware name</span>';
  }

  // search: parts, pins, nets, modules, functions
  var sInput = document.getElementById("atlas-search"), sRes = document.getElementById("atlas-search-results"), sIndex = [], sHits = [];
  function buildSearch() {
    sIndex = [];
    nodes.forEach(function (n) {
      sIndex.push({ key: (n.label + " " + (n.mpn || "") + " " + (n.value || "") + " " + (n.sub || "")).toLowerCase(), id: n.id, label: n.label, where: layerName(n) });
      (n.functions || []).forEach(function (f) { sIndex.push({ key: f.name.toLowerCase(), id: n.id, label: f.name + "()", where: n.label }); });
    });
    Object.keys(nets).forEach(function (k) {
      var n = nets[k];
      sIndex.push({ key: (n.name + " " + (n.fw ? n.fw.text : "")).toLowerCase(), net: k, label: n.name, where: "net · " + n.board });
    });
  }
  sInput.addEventListener("input", function () {
    var q = sInput.value.trim().toLowerCase();
    sRes.innerHTML = ""; sHits = [];
    if (!q) { sRes.classList.remove("open"); return; }
    var exact = [], part = [];
    sIndex.forEach(function (it) {
      if (it.key.indexOf(q) < 0) return;
      (it.label.toLowerCase() === q || it.key.split(" ")[0] === q ? exact : part).push(it);
    });
    sHits = exact.concat(part).slice(0, 10);
    sHits.forEach(function (it, i) {
      var b = document.createElement("button");
      b.type = "button";
      if (i === 0) b.className = "hl";
      b.innerHTML = '<span class="mono">' + esc(it.label) + '</span><span class="where">' + esc(it.where) + "</span>";
      b.onclick = function () { pick(it); };
      sRes.appendChild(b);
    });
    sRes.classList.toggle("open", sHits.length > 0);
  });
  function pick(it) {
    sRes.classList.remove("open");
    sInput.blur();
    if (it.net) {
      var mem = netMembers(it.net);
      selectNet(it.net, mem[0] && mem[0].part, mem[0] && mem[0].k);
      if (mem[0]) centerOn(mem[0].part, Math.max(cam.z, 1.4));
      return;
    }
    select(it.id);
    centerOn(it.id, byId[it.id].kind === "part" ? Math.max(cam.z, 1.6) : Math.max(cam.z, 0.9));
  }
  sInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && sHits[0]) pick(sHits[0]);
    if (e.key === "Escape") { sRes.classList.remove("open"); sInput.blur(); }
  });
  document.addEventListener("keydown", function (e) {
    if (!panel.classList.contains("active") || !G) return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
    if (e.key === "/") { e.preventDefault(); sInput.focus(); }
    else if (e.key === "n") gotoProblem(problemIx + 1);
    else if (e.key === "p") gotoProblem(problemIx < 0 ? problems.length - 1 : problemIx - 1);
    else if (e.key === "f") fit();
    else if (e.key === "t") animateTilt(T > 0.5 ? 0 : 1);
    else if (e.key === "Escape") select(null);
  });
  window.addEventListener("resize", function () { if (panel.classList.contains("active")) schedule(); });

  // ---------------------------------------------------------------- the tab's life
  // Loaded the first time the tab is shown, and again after the open project changes.
  function onShown() {
    var active = panel.classList.contains("active");
    if (content) content.classList.toggle("atlas-on", active);
    if (active && stale) loadIndex();
    else if (active && G) schedule();
  }
  new MutationObserver(onShown).observe(panel, { attributes: true, attributeFilter: ["class"] });
  document.addEventListener("embarch:project", function () {
    stale = true;
    atlasId = null;
    onShown();
  });
  onShown();

  // For the browser harness (tests/browser/drive_atlas.py): read-only hooks into the view.
  window.__atlas = {
    select: select, selectNet: selectNet, gotoProblem: gotoProblem, setTilt: setTilt, fit: fit,
    zoom: function (z) { cam.z = z; render(); },
    center: function (id, z) { var n = byId[id]; cam.x = n.px; cam.y = n.py; if (z) cam.z = z; render(); },
    state: function () { return { atlas: atlasId, nodes: nodes.length, edges: edges.length, sel: sel, net: selNet, z: cam.z, T: T, problems: problems.length }; }
  };
})();
