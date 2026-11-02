// embarch-ui part editor: one repo part template, edited in a dialog over the
// Topology tab and written through the bench routes.
//
// Phase 3 of embarch-bench-editor-proposal.md ("the parts bin and the repo
// part editor"). The canvas's parts bin opens it with a document event,
// `embarch:edit-part` and `detail: {id}` (null for a new part); a save
// answers with `embarch:parts-changed`, so the canvas re-reads the catalog.
//
// **What it offers is served, never restated** (spec.md's rule for every
// vocabulary): kinds, sides, categories, looks, power roles and functions come
// from `GET /api/bench/vocabulary`, and so do the rules between fields — which
// kinds take a power role (`power_kinds`), which kind carries functions (the
// hub's: a function is what a connector carries to this machine), and which
// functions take an interface or line facts — and so do the limits a template
// is held to (`template`: the connector cap, each number field's range, the
// line facts by name). **What a save refuses is the server's to say**: a
// refusal is shown verbatim, never re-worded or pre-empted by a copy of its
// checks here, so a number field is text and what was typed is what is sent.
//
// **A changed draft is never dropped silently**: Cancel or Esc asks first,
// another part opened over it asks first, and a project switch keeps it on
// screen with Save disabled. `window.embarchParts.unsaved()` says whether
// there is one, for the page's own leave guards.
//
// A built-in opens read-only, with "Copy to this repo"; a repo template the
// server could not load opens on its error, and a fresh one under that id is
// offered only after the user confirms (the server never creates over a file
// that is there, so the old one goes by hand first). The live drawing is the
// canvas's own, `window.embarchBench.previewTemplate(svg, template)`; with no
// canvas, the preview says so instead of drawing anything.
//
// Zero-build (decision 2): plain JS, its own dialog DOM, no timer.

(function () {
  "use strict";

  var ROUTE_VOCAB = "/api/bench/vocabulary";
  var ROUTE_PARTS = "/api/bench/parts";
  var PARTS_DIR = "embarch/parts/";

  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }
  function list(v) { return Array.isArray(v) ? v : []; }
  function clone(v) { return v == null ? v : JSON.parse(JSON.stringify(v)); }
  function blank(v) { return v == null || v === ""; }

  // A text line fact (a terminator, an ack) may hold control characters, so a
  // field shows it escaped: `\r\n` for CR LF. `\\` is a backslash; any other
  // backslash stays as typed.
  function showEscaped(s) {
    return String(s == null ? "" : s).replace(/\\/g, "\\\\").replace(/\r/g, "\\r").replace(/\n/g, "\\n").replace(/\t/g, "\\t");
  }
  function readEscaped(s) {
    return String(s).replace(/\\(\\|r|n|t)/g, function (_, c) {
      return c === "r" ? "\r" : c === "n" ? "\n" : c === "t" ? "\t" : "\\";
    });
  }

  // ---------------------------------------------------------------- the dialog
  var host = document.createElement("div");
  host.innerHTML =
    '<div id="part-backdrop" class="dialog-backdrop" style="display:none;"></div>' +
    '<div id="part-dialog" class="dialog dialog-wide pe-dialog" role="dialog" aria-modal="true" aria-labelledby="part-title" style="display:none;">' +
      '<div class="card-title"><span id="part-title">Part template</span><span id="part-badge" class="pe-badge" hidden></span></div>' +
      '<p id="part-path" class="placeholder-note pe-path mono"></p>' +
      '<p id="part-loading" class="placeholder-note" hidden>reading&hellip;</p>' +
      '<p id="part-stale" class="pe-problem pe-confirm pe-note pe-stale" hidden></p>' +
      '<div id="part-problem" class="pe-problem" hidden>' +
        '<pre id="part-problem-text" class="pe-problem-text mono"></pre>' +
        '<p id="part-problem-note" class="pe-note"></p>' +
      '</div>' +
      '<div id="part-confirm" class="pe-problem pe-confirm" hidden>' +
        '<p id="part-confirm-text" class="pe-note"></p>' +
        '<div class="sd-row-actions">' +
          '<button type="button" id="part-confirm-yes" class="btn btn-primary">Start fresh</button>' +
          '<button type="button" id="part-confirm-no" class="btn">Back</button>' +
        '</div>' +
      '</div>' +
      '<div id="part-form" class="pe-layout" hidden>' +
        '<div class="pe-main">' +
          '<div class="sd-form-grid pe-grid">' +
            '<label class="sd-field"><span>Id</span><input id="part-id" data-f="id" type="text" class="sd-input mono" spellcheck="false" autocomplete="off" /></label>' +
            '<label class="sd-field"><span>Label</span><input id="part-label" data-f="label" type="text" class="sd-input" spellcheck="false" autocomplete="off" /></label>' +
            '<label class="sd-field pe-wide"><span>Subtitle</span><input id="part-subtitle" data-f="subtitle" type="text" class="sd-input" spellcheck="false" autocomplete="off" /></label>' +
            '<label class="sd-field"><span>Category</span><select id="part-category" data-f="category" class="sd-input mono"></select></label>' +
            '<label class="sd-field"><span>Look</span><select id="part-look" data-f="look" class="sd-input mono"></select></label>' +
            '<div class="pe-nums">' +
              '<label class="sd-field"><span>Width</span><input id="part-width" data-f="width" type="text" inputmode="numeric" autocomplete="off" spellcheck="false" class="sd-input mono" placeholder="default" /></label>' +
              '<label class="sd-field"><span>Min height</span><input id="part-min-height" data-f="min_height" type="text" inputmode="numeric" autocomplete="off" spellcheck="false" class="sd-input mono" placeholder="none" /></label>' +
              '<label class="sd-field"><span>Rows top</span><input id="part-rows-top" data-f="rows_top" type="text" inputmode="numeric" autocomplete="off" spellcheck="false" class="sd-input mono" placeholder="none" /></label>' +
            '</div>' +
          '</div>' +
          '<div class="pe-head"><span>Connectors</span><button type="button" id="part-add-conn" class="btn btn-tiny" data-act="conn-add">+ Connector</button></div>' +
          '<div class="table-scroll pe-table-wrap">' +
            '<table class="data-table sd-table pe-conns">' +
              '<thead><tr><th class="pe-c-move"></th><th>Id</th><th>Label</th><th>Kind</th><th>Side</th><th>Power</th><th class="pe-c-max">Max cables</th><th class="pe-c-x"></th></tr></thead>' +
              '<tbody id="part-conns"></tbody>' +
            '</table>' +
          '</div>' +
          '<p id="part-conns-empty" class="placeholder-note pe-empty" hidden>no connectors</p>' +
          '<p id="part-conns-full" class="placeholder-note pe-empty" hidden></p>' +
          '<div class="pe-head"><span>Passthrough pairs</span><button type="button" id="part-add-pass" class="btn btn-tiny" data-act="pass-add">+ Pair</button></div>' +
          '<div id="part-pass" class="pe-pass"></div>' +
          '<p id="part-pass-empty" class="placeholder-note pe-empty" hidden>no passthrough pairs</p>' +
        '</div>' +
        '<div class="pe-side">' +
          '<div class="pe-head"><span>Preview</span></div>' +
          '<div class="pe-preview"><svg id="part-preview-svg" class="pe-preview-svg" aria-label="Preview of this part"></svg></div>' +
          '<p id="part-preview-note" class="placeholder-note pe-preview-note" hidden></p>' +
        '</div>' +
      '</div>' +
      '<p id="part-result" class="sd-error pe-result" style="display:none;"></p>' +
      '<p id="part-hint" class="pe-note pe-hint" hidden></p>' +
      '<div id="part-discard" class="pe-problem pe-confirm pe-discard" role="alertdialog" aria-labelledby="part-discard-text" hidden>' +
        '<p id="part-discard-text" class="pe-note"></p>' +
        '<div class="sd-row-actions">' +
          '<button type="button" id="part-discard-yes" class="btn">Discard changes</button>' +
          '<button type="button" id="part-discard-no" class="btn btn-primary">Keep editing</button>' +
        '</div>' +
      '</div>' +
      '<div class="sd-row-actions pe-actions">' +
        '<button type="button" id="part-reload" class="btn" hidden>Reload from disk</button>' +
        '<button type="button" id="part-fresh" class="btn" hidden>Start a fresh template under this id&hellip;</button>' +
        '<span class="pe-spacer"></span>' +
        '<button type="button" id="part-cancel" class="btn">Cancel</button>' +
        '<button type="button" id="part-copy" class="btn btn-primary" hidden>Copy to this repo</button>' +
        '<button type="button" id="part-save" class="btn btn-primary" hidden>Save</button>' +
      '</div>' +
    '</div>';
  while (host.firstChild) document.body.appendChild(host.firstChild);

  var dlg = document.getElementById("part-dialog");
  var backdrop = document.getElementById("part-backdrop");
  var el = {
    title: document.getElementById("part-title"),
    badge: document.getElementById("part-badge"),
    path: document.getElementById("part-path"),
    loading: document.getElementById("part-loading"),
    stale: document.getElementById("part-stale"),
    connsFull: document.getElementById("part-conns-full"),
    discard: document.getElementById("part-discard"),
    discardText: document.getElementById("part-discard-text"),
    discardYes: document.getElementById("part-discard-yes"),
    discardNo: document.getElementById("part-discard-no"),
    problem: document.getElementById("part-problem"),
    problemText: document.getElementById("part-problem-text"),
    problemNote: document.getElementById("part-problem-note"),
    confirm: document.getElementById("part-confirm"),
    confirmText: document.getElementById("part-confirm-text"),
    form: document.getElementById("part-form"),
    id: document.getElementById("part-id"),
    label: document.getElementById("part-label"),
    subtitle: document.getElementById("part-subtitle"),
    category: document.getElementById("part-category"),
    look: document.getElementById("part-look"),
    width: document.getElementById("part-width"),
    minHeight: document.getElementById("part-min-height"),
    rowsTop: document.getElementById("part-rows-top"),
    addConn: document.getElementById("part-add-conn"),
    conns: document.getElementById("part-conns"),
    connsEmpty: document.getElementById("part-conns-empty"),
    addPass: document.getElementById("part-add-pass"),
    pass: document.getElementById("part-pass"),
    passEmpty: document.getElementById("part-pass-empty"),
    svg: document.getElementById("part-preview-svg"),
    previewNote: document.getElementById("part-preview-note"),
    result: document.getElementById("part-result"),
    hint: document.getElementById("part-hint"),
    reload: document.getElementById("part-reload"),
    fresh: document.getElementById("part-fresh"),
    cancel: document.getElementById("part-cancel"),
    copy: document.getElementById("part-copy"),
    save: document.getElementById("part-save"),
  };

  // The line a save leaves behind once the dialog has closed: where the file
  // went. It sits over the canvas until dismissed or the editor opens again.
  var saved = document.createElement("div");
  saved.innerHTML = '<div id="part-saved" class="pe-saved" role="status" hidden><span id="part-saved-text" class="mono"></span>' +
    '<button type="button" id="part-saved-close" class="sd-icon-btn" aria-label="Dismiss">&times;</button></div>';
  (document.getElementById("topo-stage") || document.body).appendChild(saved.firstChild);
  var savedEl = document.getElementById("part-saved");
  var savedText = document.getElementById("part-saved-text");
  document.getElementById("part-saved-close").addEventListener("click", function () { savedEl.hidden = true; });

  // ---------------------------------------------------------------- state
  var vocab = null;      // GET /api/bench/vocabulary
  var catalog = null;    // GET /api/bench/parts: {parts, errors}
  // mode: "loading" | "failed" | "new" | "edit" | "builtin" | "error" | "missing"
  // new: `fresh` is the error entry a fresh template replaces, `from` the
  // built-in it was copied from. edit: `version` is what PUT names.
  var st = { mode: null, id: null, version: null, fresh: null, from: null, error: null, confirming: false };
  var pristine = null;   // the template as it stood when the draft was opened
  var draft = null;
  var opener = null;
  var openSeq = 0;
  var saving = false;

  function readOnly() { return st.mode === "builtin"; }
  function words(key) { return list(vocab && vocab[key]); }
  function has(key, word) { return words(key).indexOf(word) >= 0; }
  function takesPower(kind) { return has("power_kinds", kind); }
  // The limits a template file is held to, as served under `template`.
  function limits() { return (vocab && vocab.template && typeof vocab.template === "object") ? vocab.template : {}; }
  function lineFacts() {
    return list(limits().line_facts).filter(function (lf) { return lf && typeof lf.name === "string" && lf.name; });
  }
  function rangeOf(field) {
    var n = limits().numbers, r = n && n[field];
    return r && typeof r.min === "number" && typeof r.max === "number" ? r : null;
  }
  function rangeTitle(field) {
    var r = rangeOf(field);
    return r ? "a whole number, " + r.min + "\u2013" + r.max : "";
  }
  // A number field is text: a browser's number input reports unparseable
  // text as empty, which would drop the field instead of letting the server
  // say what is wrong with it.
  function numberAttrs(field) {
    var t = rangeTitle(field);
    return ' type="text" inputmode="numeric" autocomplete="off" spellcheck="false"' + (t ? ' title="' + esc(t) + '"' : "");
  }
  function carriesFunctions(kind) { return !!vocab && kind != null && kind === vocab.hub_kind; }
  function powerGroups() {
    var p = vocab && vocab.power;
    if (!p || typeof p !== "object") return [];
    return Object.keys(p).map(function (k) { return { label: k, words: list(p[k]) }; });
  }
  function servedPart(id) {
    return list(catalog && catalog.parts).filter(function (p) { return p && p.id === id; })[0] || null;
  }
  function errorOf(id) {
    return list(catalog && catalog.errors).filter(function (e) { return e && e.id != null && e.id === id; })[0] || null;
  }

  // ---------------------------------------------------------------- the draft
  // The draft is the template as the routes shape it, plus nothing: what is
  // hidden (a power role on a kind that takes none, an interface on a probe)
  // is kept while editing, so switching back restores it, and is left out
  // only when the template is built for a save or the preview.
  //
  // A passthrough end holds the connector it names by key, not by id, so a
  // connector renamed keeps its pairs and one removed takes them with it; an
  // end naming no connector keeps the id as written.
  var nextKey = 0;
  function draftFrom(t) {
    t = clone(t) || {};
    var d = {
      id: t.id == null ? "" : String(t.id),
      label: t.label == null ? "" : String(t.label),
      subtitle: t.subtitle == null ? "" : String(t.subtitle),
      category: t.category == null ? "" : String(t.category),
      look: t.look == null ? "" : String(t.look),
      hub: t.hub === true,
      width: t.width == null ? "" : String(t.width),
      min_height: t.min_height == null ? "" : String(t.min_height),
      rows_top: t.rows_top == null ? "" : String(t.rows_top),
      connectors: list(t.connectors).map(function (c) {
        c = c || {};
        return {
          _key: ++nextKey,
          id: c.id == null ? "" : String(c.id),
          label: c.label == null ? "" : String(c.label),
          kind: c.kind == null ? "" : String(c.kind),
          side: c.side == null ? "" : String(c.side),
          power: c.power == null ? "" : String(c.power),
          max_cables: c.max_cables == null ? "" : String(c.max_cables),
          functions: list(c.functions).map(function (fe) {
            fe = fe || {};
            var out = { function: fe.function == null ? "" : String(fe.function), interface: fe.interface == null ? "" : String(fe.interface) };
            // A text fact is held as its field shows it, escaped.
            lineFacts().forEach(function (lf) {
              var v = fe[lf.name];
              out[lf.name] = v == null ? "" : lf.numeric ? String(v) : showEscaped(v);
            });
            return out;
          }),
        };
      }),
    };
    d.passthrough = list(t.passthrough).map(function (p) {
      return [0, 1].map(function (k) {
        var id = String(list(p)[k] == null ? "" : list(p)[k]);
        var c = d.connectors.filter(function (x) { return x.id === id; })[0];
        return c ? { key: c._key } : { id: id };
      });
    });
    return d;
  }

  function byKey(key) { return draft.connectors.filter(function (c) { return c._key === key; })[0] || null; }
  function endId(end) {
    if (end.key == null) return end.id;
    var c = byKey(end.key);
    return c ? c.id : "";
  }

  // A number field as JSON: a whole number as itself, anything else as the
  // text typed, so the server's refusal names what is wrong with it.
  function num(v) {
    if (blank(v)) return undefined;
    var t = String(v).trim();
    return /^-?\d+$/.test(t) ? Number(t) : String(v);
  }

  function setIf(o, k, v) { if (v !== undefined) o[k] = v; }

  // The template as a save sends it and the preview draws it.
  function template() {
    var d = draft, t = { id: d.id, label: d.label };
    if (!blank(d.subtitle)) t.subtitle = d.subtitle;
    if (!blank(d.category)) t.category = d.category;
    if (!blank(d.look)) t.look = d.look;
    if (d.hub) t.hub = true;
    setIf(t, "width", num(d.width));
    setIf(t, "min_height", num(d.min_height));
    setIf(t, "rows_top", num(d.rows_top));
    t.passthrough = d.passthrough.map(function (p) { return [endId(p[0]), endId(p[1])]; });
    t.connectors = d.connectors.map(function (c) {
      var o = { id: c.id, label: c.label };
      if (!blank(c.kind)) o.kind = c.kind;
      if (!blank(c.side)) o.side = c.side;
      if (!blank(c.power) && takesPower(c.kind)) o.power = c.power;
      setIf(o, "max_cables", num(c.max_cables));
      if (carriesFunctions(c.kind)) {
        o.functions = c.functions.map(function (fe) {
          var e = {};
          if (!blank(fe.function)) e.function = fe.function;
          if (has("interface_functions", fe.function)) setIf(e, "interface", num(fe.interface));
          if (has("line_functions", fe.function)) {
            lineFacts().forEach(function (lf) {
              if (blank(fe[lf.name])) return;
              e[lf.name] = lf.numeric ? num(fe[lf.name]) : readEscaped(fe[lf.name]);
            });
          }
          return e;
        });
      }
      return o;
    });
    return t;
  }

  // ---------------------------------------------------------------- rendering
  // A select of served words. A value the list lacks is kept, marked and
  // disabled, so what the template said stays visible and a save sends it
  // for the server to refuse; nothing is silently swapped for a served word.
  function options(wordsList, value, placeholder) {
    var out = "";
    if (blank(value) && placeholder) out += '<option value="" disabled selected>' + esc(placeholder) + "</option>";
    if (!blank(value) && wordsList.indexOf(value) < 0) {
      out += '<option value="' + esc(value) + '" disabled selected data-unknown="1">' + esc(value) + " (not in this build)</option>";
    }
    wordsList.forEach(function (w) {
      out += '<option value="' + esc(w) + '"' + (w === value ? " selected" : "") + ">" + esc(w) + "</option>";
    });
    return out;
  }

  function powerOptions(value) {
    var all = [];
    var out = '<option value=""' + (blank(value) ? " selected" : "") + ">none</option>";
    powerGroups().forEach(function (g) {
      out += '<optgroup label="' + esc(g.label) + '">';
      g.words.forEach(function (w) {
        all.push(w);
        out += '<option value="' + esc(w) + '"' + (w === value ? " selected" : "") + ">" + esc(w) + "</option>";
      });
      out += "</optgroup>";
    });
    if (!blank(value) && all.indexOf(value) < 0) {
      out = '<option value="' + esc(value) + '" disabled selected data-unknown="1">' + esc(value) + " (not in this build)</option>" + out;
    }
    return out;
  }

  function dis() { return readOnly() ? " disabled" : ""; }

  function connectorRows() {
    var n = draft.connectors.length;
    return draft.connectors.map(function (c, i) {
      var power = takesPower(c.kind)
        ? '<select data-f="power" aria-label="Power role"' + dis() + ">" + powerOptions(c.power) + "</select>"
        : '<span class="pe-na" title="this kind carries no power">&mdash;</span>';
      var row =
        '<tr data-row="' + i + '" class="pe-conn">' +
          '<td class="pe-c-move"><button type="button" class="sd-icon-btn" data-act="up" title="Move up" aria-label="Move up"' + (i === 0 ? " disabled" : "") + ">&uarr;</button>" +
            '<button type="button" class="sd-icon-btn" data-act="down" title="Move down" aria-label="Move down"' + (i === n - 1 ? " disabled" : "") + ">&darr;</button></td>" +
          '<td><input type="text" data-f="id" aria-label="Connector id" spellcheck="false" autocomplete="off" value="' + esc(c.id) + '"' + dis() + " /></td>" +
          '<td><input type="text" data-f="label" aria-label="Connector label" spellcheck="false" autocomplete="off" value="' + esc(c.label) + '"' + dis() + " /></td>" +
          '<td><select data-f="kind" aria-label="Kind"' + dis() + ">" + options(words("kinds"), c.kind, "pick one") + "</select></td>" +
          '<td><select data-f="side" aria-label="Side"' + dis() + ">" + options(words("sides"), c.side, "pick one") + "</select></td>" +
          "<td>" + power + "</td>" +
          '<td class="pe-c-max"><input' + numberAttrs("max_cables") + ' data-f="max_cables" aria-label="Max cables" placeholder="default" value="' + esc(c.max_cables) + '"' + dis() + " /></td>" +
          '<td class="pe-c-x"><button type="button" class="btn-icon-danger btn" data-act="remove" title="Remove this connector" aria-label="Remove this connector">&times;</button></td>' +
        "</tr>";
      if (carriesFunctions(c.kind)) row += functionRow(c, i);
      return row;
    }).join("");
  }

  // What a connector to this machine carries: one line per function, with an
  // interface where the vocabulary says the function takes one and the line
  // facts where it takes those, under the TOML's own names.
  function functionRow(c, i) {
    var entries = c.functions.map(function (fe, j) {
      var facts = "";
      if (has("interface_functions", fe.function)) {
        facts += '<label class="sd-param pe-fact"><span>interface</span><input' + numberAttrs("interface") + ' data-f="interface" placeholder="unknown" value="' + esc(fe.interface) + '"' + dis() + " /></label>";
      }
      if (has("line_functions", fe.function)) {
        lineFacts().forEach(function (lf) {
          var attrs = lf.numeric ? numberAttrs(lf.name)
            : ' type="text" spellcheck="false" autocomplete="off" title="escaped: \\r is CR, \\n is LF, \\t is tab, \\\\ is a backslash"';
          facts += '<label class="sd-param pe-fact' + (lf.numeric ? "" : " pe-fact-text") + '"><span>' + esc(lf.name) + "</span>" +
            "<input" + attrs + ' data-f="' + esc(lf.name) + '" value="' + esc(fe[lf.name]) + '"' + dis() + " /></label>";
        });
      }
      return '<div class="pe-fn" data-fn="' + j + '">' +
        '<label class="sd-param pe-fact pe-fact-fn"><span>function</span><select data-f="function"' + dis() + ">" + options(words("functions"), fe.function, "pick one") + "</select></label>" +
        facts +
        '<button type="button" class="sd-icon-btn pe-fn-x" data-act="fn-remove" title="Remove this function" aria-label="Remove this function">&times;</button>' +
        "</div>";
    }).join("");
    return '<tr data-row="' + i + '" class="pe-fn-row"><td></td><td colspan="7">' +
      '<div class="pe-fns"><span class="pe-fns-label">carries to this machine</span>' +
      (entries || '<span class="placeholder-note pe-none">nothing declared</span>') +
      '<button type="button" class="btn btn-tiny" data-act="fn-add">+ Function</button></div></td></tr>';
  }

  // A passthrough end picks among the connectors whose kind makes up a bus.
  // An end naming anything else is kept and marked, for the server to refuse.
  function passRows() {
    var bus = draft.connectors.filter(function (c) { return takesPower(c.kind); });
    function name(c) { return blank(c.id) ? "(no id yet)" : c.id; }
    return draft.passthrough.map(function (p, i) {
      function end(k) {
        var v = p[k], out = "", c = v.key != null ? byKey(v.key) : null;
        if (v.key == null && blank(v.id)) out += '<option value="" disabled selected>pick one</option>';
        if (v.key == null && !blank(v.id)) {
          out += '<option value="' + esc(v.id) + '" selected disabled data-unknown="1">' + esc(v.id) + " (no such connector)</option>";
        }
        if (c && bus.indexOf(c) < 0) {
          out += '<option value="k' + c._key + '" selected disabled data-unknown="1">' + esc(name(c)) + " (carries no power)</option>";
        }
        bus.forEach(function (b) {
          out += '<option value="k' + b._key + '"' + (c === b ? " selected" : "") + ">" + esc(name(b)) + "</option>";
        });
        return '<select class="sd-input mono" data-f="end" data-end="' + k + '" aria-label="Passthrough end ' + (k + 1) + '"' + dis() + ">" + out + "</select>";
      }
      return '<div class="pe-pass-row" data-pass="' + i + '">' + end(0) + '<span class="pe-pass-join">&harr;</span>' + end(1) +
        '<button type="button" class="btn-icon-danger btn" data-act="pass-remove" title="Remove this pair" aria-label="Remove this pair">&times;</button></div>';
    }).join("");
  }

  // The served connector cap: at it, Add is disabled and says why.
  function atCap() {
    var max = limits().max_connectors;
    return typeof max === "number" && draft.connectors.length >= max;
  }
  function renderConnectors() {
    el.conns.innerHTML = connectorRows();
    el.connsEmpty.hidden = draft.connectors.length > 0;
    var full = atCap();
    el.addConn.disabled = full;
    el.connsFull.hidden = !full;
    el.connsFull.textContent = full ? "A template holds at most " + limits().max_connectors + " connectors." : "";
  }
  function renderPass() {
    el.pass.innerHTML = passRows();
    el.passEmpty.hidden = draft.passthrough.length > 0;
  }

  function renderPath() {
    if (st.mode === "builtin") {
      el.path.textContent = "built into embarch-ui · read-only";
      return;
    }
    var id = draft ? draft.id : st.id;
    el.path.textContent = PARTS_DIR + (blank(id) ? "<id>" : id) + ".toml";
  }

  function renderForm() {
    var ro = readOnly();
    el.id.value = draft.id;
    el.label.value = draft.label;
    el.subtitle.value = draft.subtitle;
    el.category.innerHTML = options(words("categories"), draft.category, "pick one");
    el.look.innerHTML = options(words("looks"), draft.look, "pick one");
    el.width.value = draft.width;
    el.minHeight.value = draft.min_height;
    el.rowsTop.value = draft.rows_top;
    el.width.title = rangeTitle("width");
    el.minHeight.title = rangeTitle("min_height");
    el.rowsTop.title = rangeTitle("rows_top");
    // The id names the file, so it is fixed once the file exists — and for a
    // fresh template replacing an unreadable one, it is that file's.
    el.id.disabled = ro || st.mode === "edit" || !!st.fresh;
    el.id.title = el.id.disabled && !ro ? "fixed: it names the file" : "";
    [el.label, el.subtitle, el.category, el.look, el.width, el.minHeight, el.rowsTop].forEach(function (x) { x.disabled = ro; });
    renderConnectors();
    renderPass();
    renderPath();
    preview();
  }

  // The canvas draws the preview; this file draws nothing of its own.
  function preview() {
    var api = window.embarchBench;
    el.svg.innerHTML = "";
    el.previewNote.hidden = true;
    if (!api || typeof api.previewTemplate !== "function") {
      el.previewNote.textContent = "The bench canvas did not load, so there is no preview.";
      el.previewNote.hidden = false;
      dlg.setAttribute("data-preview", "none");
      return;
    }
    try {
      api.previewTemplate(el.svg, template());
      dlg.setAttribute("data-preview", "drawn");
    } catch (e) {
      el.svg.innerHTML = "";
      el.previewNote.textContent = "The canvas could not draw this template: " + (e && e.message ? e.message : String(e));
      el.previewNote.hidden = false;
      dlg.setAttribute("data-preview", "failed");
    }
  }

  function showResult(text) {
    el.result.textContent = text || "";
    el.result.style.display = text ? "block" : "none";
  }
  function showHint(text) {
    el.hint.textContent = text || "";
    el.hint.hidden = !text;
  }

  // One place decides what is on screen for each mode.
  function render() {
    var m = st.mode;
    dlg.setAttribute("data-mode", m || "");
    dlg.classList.toggle("pe-ro", m === "builtin");
    el.loading.hidden = m !== "loading";
    var titles = {
      loading: "Part template",
      failed: "Part template",
      new: st.fresh ? "A fresh template for an unreadable file" : st.from ? "New part template, copied from " + st.from : "New part template",
      edit: "Part template",
      builtin: "Built-in part",
      error: "Part template that will not load",
      missing: "Part template",
    };
    el.title.textContent = titles[m] || "Part template";
    var badge = m === "builtin" ? "built-in" : m === "edit" ? "this repo" : m === "new" ? "new" : m === "error" ? "unreadable" : "";
    el.badge.textContent = badge;
    el.badge.hidden = !badge;
    el.badge.className = "pe-badge" + (m === "error" ? " pe-badge-bad" : "");

    var formOn = m === "new" || m === "edit" || m === "builtin";
    el.form.hidden = !formOn;
    el.problem.hidden = !(m === "error" || m === "failed" || m === "missing" || (m === "builtin" && st.error) || (m === "new" && st.fresh));
    el.confirm.hidden = !(m === "error" && st.confirming);
    el.path.hidden = m === "loading" || m === "failed";
    if (formOn) renderForm(); else renderPath();

    el.save.hidden = !(m === "new" || m === "edit");
    el.copy.hidden = m !== "builtin";
    el.fresh.hidden = !(m === "error" && !st.confirming && freshAllowed());
    el.reload.hidden = !(m === "missing" || m === "failed");
    el.cancel.textContent = m === "new" || m === "edit" ? "Cancel" : "Close";
    el.save.disabled = saving || !!st.stale;
    renderProblem();
    renderGuard();
  }

  function builtinWithId(id) {
    var p = servedPart(id);
    return p && p.source === "builtin" ? p : null;
  }

  // A fresh template is offered under an unreadable file's id unless that id
  // is a built-in's: a repo template may never take one.
  function freshAllowed() { return !!st.error && !builtinWithId(st.id); }

  function renderProblem() {
    var m = st.mode, e = st.error;
    if (m === "failed" || m === "missing") {
      el.problemText.textContent = st.failure || "";
      el.problemText.hidden = !st.failure;
      el.problemNote.textContent = m === "missing"
        ? "No part template `" + st.id + "` is listed now; its file may have been removed or renamed."
        : "";
      el.problemNote.hidden = m !== "missing";
      return;
    }
    if (!e) return;
    el.problemText.textContent = e.error || "";
    el.problemText.hidden = false;
    el.problemNote.hidden = false;
    if (m === "builtin") {
      el.problemNote.textContent = (e.file || PARTS_DIR + st.id + ".toml") + " is not used: a repo template cannot take a built-in's id.";
    } else if (m === "new") {
      el.problemNote.textContent = "Saving creates " + PARTS_DIR + st.id + ".toml, and the server never creates over a file that is there: " +
        "remove or fix the unreadable one by hand first.";
    } else if (builtinWithId(st.id)) {
      el.problemNote.textContent = (e.file || PARTS_DIR + st.id + ".toml") + " will not load, and `" + st.id + "` is a built-in's id, " +
        "which a repo template may not take: give the file another id and name by hand, or remove it.";
    } else {
      el.problemNote.textContent = (e.file || PARTS_DIR + st.id + ".toml") + " will not load. Fix it by hand, or start a fresh template under its id.";
    }
    el.confirmText.textContent = "A fresh template under `" + st.id + "` would replace " + (e.file || PARTS_DIR + st.id + ".toml") +
      ". Saving creates the file, and the server refuses to create over one that is there, so the unreadable file must be " +
      "removed or fixed by hand before Save will go through. Nothing is written until you Save.";
  }

  // ---------------------------------------------------------------- the unsaved-draft guard
  // A draft is changed when the template it builds differs from the one it
  // was opened as; typing a field back to what it was is no change.
  function snapshot() { return draft ? JSON.stringify(template()) : null; }
  function markPristine() { pristine = snapshot(); }
  function changed() { return !!draft && !readOnly() && snapshot() !== pristine; }

  function draftName() {
    var id = draft && !blank(draft.id) ? draft.id : null;
    return id ? "`" + id + "`" : "this new part";
  }

  function renderGuard() {
    el.discard.hidden = !st.discarding;
    if (st.discarding) {
      var next = st.then ? (st.then.id == null ? " and open a new part" : " and open `" + st.then.id + "`") : "";
      el.discardText.textContent = draftName() + " has changes that are not saved. Discard them" + next + "?";
    }
    el.stale.hidden = !st.stale;
    el.stale.textContent = st.stale
      ? "The open project changed, so this draft belongs to the repo that was open before and cannot be saved here. " +
        "Copy what you need from it, then close it."
      : "";
  }

  // Asks before a changed draft goes. `then` is what to do once it is
  // discarded: null closes, `{id}` opens that part (null id: a new one).
  function askDiscard(then) {
    st.discarding = true;
    st.then = then || null;
    if (!st.back) st.back = document.activeElement;
    renderGuard();
    el.discard.scrollIntoView({ block: "nearest" });
    // After app.js's Esc handler, which moves focus to a panel under the
    // dialog once it has clicked Cancel.
    Promise.resolve().then(function () {
      if (st.discarding) el.discardNo.focus({ preventScroll: true });
    });
  }

  function keepEditing() {
    var back = st.back;
    st.discarding = false;
    st.then = null;
    st.back = null;
    renderGuard();
    Promise.resolve().then(function () {
      if (back && back.isConnected && dlg.contains(back) && !back.disabled && back.getClientRects().length) back.focus({ preventScroll: true });
      else focusFirst();
    });
  }

  // Cancel and Esc: a changed draft asks first; Esc while it asks is "keep
  // editing", the topmost thing closing first (decision 78).
  function requestClose() {
    if (st.discarding) { keepEditing(); return; }
    if (changed()) { askDiscard(null); return; }
    close();
  }

  function discardNow() {
    var then = st.then;
    if (then) open(then.id);
    else close();
  }

  window.embarchParts = {
    // A dialog open on a draft that differs from what it opened as.
    unsaved: function () { return shown() && changed(); },
  };

  // ---------------------------------------------------------------- open, close
  function api(method, route, body) {
    var init = { method: method, headers: { Accept: "application/json" } };
    if (body !== undefined) {
      init.headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    return fetch(route, init).then(function (r) {
      return r.text().then(function (t) {
        if (!r.ok) {
          var err = new Error(t || "HTTP " + r.status);
          err.status = r.status;
          throw err;
        }
        try {
          return t ? JSON.parse(t) : null;
        } catch (_) {
          var bad = new Error(method + " " + route + " answered something that is not JSON");
          bad.status = r.status;
          throw bad;
        }
      });
    }, function (e) {
      var err = new Error("could not reach embarch-ui: " + (e && e.message ? e.message : String(e)));
      err.status = 0;
      throw err;
    });
  }

  function shown() { return dlg.style.display === "block"; }

  function focusFirst() {
    var order = st.mode === "builtin" ? [el.copy] : st.mode === "error" ? [el.fresh, el.cancel]
      : st.mode === "new" ? [el.id.disabled ? el.label : el.id] : st.mode === "edit" ? [el.label] : [el.reload, el.cancel];
    var target = order.filter(function (x) { return x && !x.hidden && !x.disabled && x.getClientRects().length; })[0] || el.cancel;
    target.focus({ preventScroll: true });
  }

  // A save still in flight from before answers into nothing (`openSeq`), so
  // the Save it disabled is enabled here, not by that answer.
  function reset(mode, id) {
    st = { mode: mode, id: id, version: null, fresh: null, from: null, error: null, confirming: false, failure: null,
           discarding: false, then: null, back: null, stale: false };
    saving = false;
    el.save.disabled = false;
    dlg.removeAttribute("data-busy");
    showResult("");
    showHint("");
  }

  // Every open reads the vocabulary and the catalog again: the version a save
  // names must be the file's now, not the bin's from a minute ago.
  function open(id) {
    var seq = ++openSeq;
    if (!shown()) opener = document.activeElement;
    savedEl.hidden = true;
    reset("loading", id);
    draft = null;
    backdrop.style.display = "block";
    dlg.style.display = "block";
    dlg.scrollTop = 0;
    render();
    return Promise.all([api("GET", ROUTE_VOCAB), api("GET", ROUTE_PARTS)]).then(function (res) {
      if (seq !== openSeq) return;
      vocab = res[0] || {};
      catalog = res[1] || { parts: [], errors: [] };
      decide(id);
      markPristine();
      render();
      focusFirst();
    }).catch(function (e) {
      if (seq !== openSeq) return;
      st.mode = "failed";
      st.failure = e && e.message ? e.message : String(e);
      render();
      focusFirst();
    });
  }

  function decide(id) {
    if (id == null) {
      st.mode = "new";
      draft = draftFrom({});
      return;
    }
    var part = servedPart(id);
    st.error = errorOf(id);
    if (part && part.source === "builtin") {
      st.mode = "builtin";
      draft = draftFrom(part);
    } else if (part) {
      st.mode = "edit";
      st.version = part.version == null ? null : String(part.version);
      draft = draftFrom(part);
    } else if (st.error) {
      st.mode = "error";
    } else {
      st.mode = "missing";
      st.failure = "";
    }
  }

  function close() {
    ++openSeq;
    dlg.style.display = "none";
    backdrop.style.display = "none";
    draft = null;
    var back = opener;
    opener = null;
    if (back && back.isConnected && typeof back.focus === "function" && !back.disabled && back.getClientRects().length) {
      back.focus({ preventScroll: true });
    }
  }

  // ---------------------------------------------------------------- built-in copy, fresh start
  function takenIds() {
    var ids = Object.create(null);
    list(catalog && catalog.parts).forEach(function (p) { if (p && p.id != null) ids[p.id] = true; });
    list(catalog && catalog.errors).forEach(function (e) { if (e && e.id != null) ids[e.id] = true; });
    return ids;
  }

  function copyBuiltin() {
    var part = servedPart(st.id);
    if (!part) return;
    var taken = takenIds(), id = part.id + "-copy", n = 2;
    while (taken[id]) id = part.id + "-copy-" + n++;
    var label = part.label;
    reset("new", null);
    st.from = label;
    draft = draftFrom(part);
    draft.id = id;
    markPristine();
    render();
    el.id.focus({ preventScroll: true });
    el.id.select();
  }

  function startFresh() {
    var e = st.error, id = st.id;
    reset("new", id);
    st.fresh = e;
    st.error = e;
    draft = draftFrom({ id: id });
    markPristine();
    render();
    focusFirst();
  }

  // ---------------------------------------------------------------- save
  // A file written is announced even when the dialog was closed or reopened
  // while the request was out: the canvas must re-read what is on disk.
  function onSaved(id, created, current) {
    var file = PARTS_DIR + id + ".toml";
    document.dispatchEvent(new CustomEvent("embarch:parts-changed", { detail: { id: id, file: file } }));
    if (current) close();
    savedText.textContent = (created ? "Created " : "Saved ") + file;
    savedEl.hidden = false;
  }

  function save() {
    if (saving || st.stale || !(st.mode === "new" || st.mode === "edit")) return;
    var body = template();
    var put = st.mode === "edit";
    if (put) body.version = st.version;
    var route = put ? ROUTE_PARTS + "/" + encodeURIComponent(st.id) : ROUTE_PARTS;
    var seq = openSeq;
    saving = true;
    el.save.disabled = true;
    dlg.setAttribute("data-busy", "1");
    showResult("");
    showHint("");
    el.reload.hidden = true;
    api(put ? "PUT" : "POST", route, body).then(function () {
      onSaved(body.id, !put, seq === openSeq);
    }).catch(function (e) {
      if (seq !== openSeq) return;
      showResult(e && e.message ? e.message : String(e));
      var status = e && e.status;
      // A changed or vanished file: what is on disk now is the way on.
      if (put && (status === 409 || status === 404)) {
        el.reload.hidden = false;
        showHint("Reload from disk replaces what is in this dialog with the file as it is now.");
      } else if (!put && st.fresh && status === 409 && errorOf(draft.id)) {
        showHint(PARTS_DIR + draft.id + ".toml is still there and will not load: remove it or fix it by hand, then Save again.");
      }
    }).then(function () {
      if (seq !== openSeq) return;
      saving = false;
      el.save.disabled = !!st.stale;
      dlg.removeAttribute("data-busy");
    });
  }

  // ---------------------------------------------------------------- editing
  function rowOf(t) { var r = t.closest && t.closest("[data-row]"); return r ? +r.getAttribute("data-row") : -1; }
  function fnOf(t) { var r = t.closest && t.closest("[data-fn]"); return r ? +r.getAttribute("data-fn") : -1; }
  function passOf(t) { var r = t.closest && t.closest("[data-pass]"); return r ? +r.getAttribute("data-pass") : -1; }

  // After a structural change the rows are rebuilt; focus goes back to the
  // same control, on the row it moved to.
  function refocus(row, fn, f, act) {
    var sel = '[data-row="' + row + '"]' + (fn >= 0 ? ' [data-fn="' + fn + '"]' : "") + " " + (act ? '[data-act="' + act + '"]' : '[data-f="' + f + '"]');
    var target = dlg.querySelector(sel);
    if (target && target.disabled) target = dlg.querySelector('[data-row="' + row + '"] [data-act="' + (act === "up" ? "down" : "up") + '"]');
    if (target && !target.disabled) target.focus({ preventScroll: true });
  }

  function onField(t, structural) {
    var f = t.getAttribute("data-f");
    if (!f || !draft || readOnly()) return;
    var row = rowOf(t), fn = fnOf(t), pass = passOf(t);
    if (pass >= 0) {
      var v = t.value, k = /^k\d+$/.test(v) ? +v.slice(1) : null;
      draft.passthrough[pass][+t.getAttribute("data-end")] = k != null && byKey(k) ? { key: k } : { id: v };
      preview();
      return;
    }
    if (row < 0) {
      draft[f] = t.value;
      if (f === "id") renderPath();
      preview();
      return;
    }
    var c = draft.connectors[row];
    if (!c) return;
    if (fn >= 0) {
      var fe = c.functions[fn];
      if (!fe) return;
      fe[f] = t.value;
      if (f === "function" && structural) { renderConnectors(); refocus(row, fn, "function"); }
      preview();
      return;
    }
    c[f] = t.value;
    if (f === "id") renderPass();   // a pair names its ends by id
    if (f === "kind" && structural) {
      renderConnectors();
      renderPass();
      refocus(row, -1, "kind");
    }
    preview();
  }

  dlg.addEventListener("input", function (ev) {
    var t = ev.target;
    if (t.tagName === "SELECT") return;
    onField(t, false);
  });
  // A select changes on `change`; a text field on `input`, and on `change` as
  // well, which is all some edits fire (a field cleared by the browser).
  dlg.addEventListener("change", function (ev) {
    var t = ev.target;
    onField(t, t.tagName === "SELECT");
  });

  function newConnector() {
    var prev = draft.connectors[draft.connectors.length - 1];
    var kinds = words("kinds"), sides = words("sides");
    return {
      _key: ++nextKey, id: "", label: "",
      kind: prev ? prev.kind : kinds[0] || "",
      side: prev ? prev.side : sides[0] || "",
      power: "", max_cables: "", functions: [],
    };
  }

  function newFunction(c) {
    var used = c.functions.map(function (fe) { return fe.function; });
    var free = words("functions").filter(function (w) { return used.indexOf(w) < 0; });
    var fe = { function: free[0] || "", interface: "" };
    lineFacts().forEach(function (lf) { fe[lf.name] = ""; });
    return fe;
  }

  dlg.addEventListener("click", function (ev) {
    var b = ev.target.closest && ev.target.closest("[data-act]");
    if (!b || b.disabled || !draft || readOnly()) return;
    var act = b.getAttribute("data-act"), row = rowOf(b), fn = fnOf(b), pass = passOf(b);
    var cs = draft.connectors;
    if (act === "conn-add") {
      if (atCap()) return;
      cs.push(newConnector());
      renderConnectors();
      renderPass();
      refocus(cs.length - 1, -1, "id");
    } else if (act === "remove" && row >= 0) {
      var gone = cs.splice(row, 1)[0];
      draft.passthrough = draft.passthrough.filter(function (p) { return p[0].key !== gone._key && p[1].key !== gone._key; });
      renderConnectors();
      renderPass();
      if (cs.length) refocus(Math.min(row, cs.length - 1), -1, null, "remove");
      else el.addConn.focus({ preventScroll: true });
    } else if ((act === "up" || act === "down") && row >= 0) {
      var to = act === "up" ? row - 1 : row + 1;
      if (to < 0 || to >= cs.length) return;
      var moved = cs.splice(row, 1)[0];
      cs.splice(to, 0, moved);
      renderConnectors();
      refocus(to, -1, null, act);
    } else if (act === "fn-add" && row >= 0) {
      cs[row].functions.push(newFunction(cs[row]));
      renderConnectors();
      refocus(row, cs[row].functions.length - 1, "function");
    } else if (act === "fn-remove" && row >= 0 && fn >= 0) {
      cs[row].functions.splice(fn, 1);
      renderConnectors();
      refocus(row, -1, null, "fn-add");
    } else if (act === "pass-add") {
      draft.passthrough.push([{ id: "" }, { id: "" }]);
      renderPass();
      var ends = el.pass.querySelectorAll('[data-pass="' + (draft.passthrough.length - 1) + '"] select');
      if (ends[0]) ends[0].focus({ preventScroll: true });
    } else if (act === "pass-remove" && pass >= 0) {
      draft.passthrough.splice(pass, 1);
      renderPass();
      el.addPass.focus({ preventScroll: true });
    } else {
      return;
    }
    preview();
  });

  el.save.addEventListener("click", save);
  el.cancel.addEventListener("click", requestClose);
  el.discardYes.addEventListener("click", discardNow);
  el.discardNo.addEventListener("click", keepEditing);
  el.copy.addEventListener("click", copyBuiltin);
  el.reload.addEventListener("click", function () { open(st.id); });
  el.fresh.addEventListener("click", function () {
    st.confirming = true;
    render();
    document.getElementById("part-confirm-no").focus({ preventScroll: true });
  });
  document.getElementById("part-confirm-yes").addEventListener("click", startFresh);
  document.getElementById("part-confirm-no").addEventListener("click", function () {
    st.confirming = false;
    render();
    focusFirst();
  });
  // Enter in a text field saves, as the other Topology dialogs' Save does.
  dlg.addEventListener("keydown", function (ev) {
    if (ev.key !== "Enter" || ev.target.tagName !== "INPUT" || el.save.hidden || el.save.disabled) return;
    ev.preventDefault();
    save();
  });

  // Esc is app.js's: this dialog is listed with the Topology tab's dialogs,
  // so it closes before a panel under it, and never under Core's prompt.

  document.addEventListener("embarch:edit-part", function (ev) {
    var d = ev && ev.detail;
    var id = d && d.id != null ? String(d.id) : null;
    if (shown() && changed()) askDiscard({ id: id });
    else open(id);
  });
  // The open repo changed under the dialog: what it holds describes the old
  // one. A changed draft stays, unsaveable and saying so, until it is closed.
  document.addEventListener("embarch:project", function () {
    savedEl.hidden = true;
    if (!shown()) return;
    if (!changed()) { close(); return; }
    ++openSeq;   // nothing read for the old repo answers into this dialog now
    saving = false;
    dlg.removeAttribute("data-busy");
    st.stale = true;
    showHint("");
    el.reload.hidden = true;
    render();
  });
})();
