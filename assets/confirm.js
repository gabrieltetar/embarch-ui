// embarch-ui Confirm tab: what waits for the engineer's confirmation, drawn from
// Core's own card for each item (decision 110). Every string here came from an
// agent, so each one is set with textContent and never parsed as markup. The
// nonce rides with the card Core issued for this rendering and goes back with
// the confirmation; a refusal shows Core's reason and reloads the cards.

(function () {
  "use strict";

  var panel = document.querySelector('.tab-panel[data-tab="confirm"]');
  if (!panel) return;
  var listEl = document.getElementById("cf-list");
  var stateEl = document.getElementById("cf-state");
  var allBtn = document.getElementById("cf-all");
  var reloadBtn = document.getElementById("cf-reload");
  var items = [];
  var busy = false;

  function make(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }

  // A value as quoted text: a string as it is, anything else as indented JSON.
  function quoted(v) {
    return make("pre", "cf-quote", typeof v === "string" ? v : JSON.stringify(v, null, 2));
  }

  function section(title, v) {
    var s = make("div", "cf-sec");
    s.appendChild(make("h4", null, title));
    s.appendChild(quoted(v));
    return s;
  }

  // What Core rendered, section by section where the definition names one and
  // whole otherwise: nothing is dropped, so what is shown is what is confirmed.
  function body(it) {
    var wrap = make("div", "cf-body");
    var card = it.card || {};
    var def = card.definition;
    if (card.diff !== undefined && card.diff !== null) wrap.appendChild(section("Difference from the premade test", card.diff));
    if (def && typeof def === "object" && !Array.isArray(def)) {
      var shown = {};
      [["derived_from", "Derived from"], ["prompts", "Prompts"], ["provenance", "Provenance"],
       ["limits", "Limits"], ["mapping", "Hook mapping"], ["inputs", "Inputs"]].forEach(function (p) {
        if (def[p[0]] !== undefined) { wrap.appendChild(section(p[1], def[p[0]])); shown[p[0]] = true; }
      });
      wrap.appendChild(section("Definition", def));
    } else if (def !== undefined) {
      wrap.appendChild(section("Definition", def));
    }
    return wrap;
  }

  function setState(text) { stateEl.textContent = text; }

  function shown() { return items.filter(function (i) { return i.nonce && !i.done; }); }

  function refreshControls() {
    allBtn.disabled = busy || shown().length === 0;
    reloadBtn.disabled = busy;
    Array.prototype.forEach.call(listEl.querySelectorAll("button.cf-confirm"), function (b) { b.disabled = busy; });
  }

  function draw() {
    listEl.textContent = "";
    if (!items.length) listEl.appendChild(make("p", "placeholder-note", "Nothing waits for a confirmation."));
    items.forEach(function (it) {
      var c = make("div", "cf-card");
      var head = make("div", "cf-head");
      head.appendChild(make("span", "badge", it.kind || "?"));
      head.appendChild(make("span", "cf-name", it.name || it.hash));
      if (it.done) head.appendChild(make("span", "badge badge-success", "confirmed"));
      else if (it.nonce) {
        var b = make("button", "btn btn-primary cf-confirm", "Confirm");
        b.type = "button";
        b.addEventListener("click", function () { confirmOne(it); });
        head.appendChild(b);
      }
      c.appendChild(head);
      c.appendChild(make("div", "placeholder-note", "hash " + it.hash + (it.confirmation ? " · " + it.confirmation : "")));
      if (it.error) c.appendChild(make("div", "cf-err", it.error));
      else if (it.card) c.appendChild(body(it));
      if (it.refused) c.appendChild(make("div", "cf-err", it.refused));
      listEl.appendChild(c);
    });
    refreshControls();
  }

  async function load(keepRefusals) {
    var refused = {};
    if (keepRefusals) items.forEach(function (i) { if (i.refused) refused[i.hash] = i.refused; });
    setState("Loading...");
    try {
      var resp = await fetch("/api/confirmations");
      if (!resp.ok) { setState("Could not read Core: " + (await resp.text())); return; }
      items = (await resp.json()).items || [];
      items.forEach(function (i) { if (refused[i.hash]) i.refused = refused[i.hash]; });
      setState(items.length + " waiting");
    } catch (e) {
      setState("Could not read Core: " + (e && e.message ? e.message : String(e)));
      return;
    }
    draw();
  }

  // One confirmation, with the nonce Core issued for the card as drawn. Resolves
  // true when Core recorded it; a refusal is kept on the item as Core's reason.
  async function post(it) {
    it.refused = "";
    var resp = await fetch("/api/confirmations", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hash: it.hash, nonce: it.nonce }),
    });
    if (resp.ok) { it.done = true; return true; }
    it.refused = "Refused (" + resp.status + "): " + (await resp.text());
    return false;
  }

  async function run(list) {
    if (busy) return;
    busy = true;
    refreshControls();
    var failed = false;
    try {
      for (var i = 0; i < list.length; i++) {
        if (!(await post(list[i]))) failed = true;
      }
    } catch (e) {
      failed = true;
      setState("The confirmation was not sent: " + (e && e.message ? e.message : String(e)));
    }
    busy = false;
    // A spent or refused nonce is useless: draw what Core holds now, keeping the reasons.
    if (failed) { await load(true); return; }
    await load(false);
  }

  function confirmOne(it) { run([it]); }

  allBtn.addEventListener("click", function () { run(shown()); });
  reloadBtn.addEventListener("click", function () { load(false); });
  document.addEventListener("click", function (ev) {
    var t = ev.target && ev.target.closest ? ev.target.closest('.nav-item[data-tab="confirm"]') : null;
    if (t) load(false);
  });

  // ---- The go card (decision 111) --------------------------------------------
  // Core's card for a prepared Test run, drawn from Core's own data. Every string an
  // agent wrote arrives as {"agent_text": s} and is drawn as quoted text. The go posts
  // the card's run id, hash and nonce with the person's token; a refusal is shown as
  // Core's text and the card is read again for a fresh nonce, never retried here.
  var gcRun = document.getElementById("gc-run");
  var gcLoad = document.getElementById("gc-load");
  var gcState = document.getElementById("gc-state");
  var gcCard = document.getElementById("gc-card");
  var gcBusy = false;

  // A value with every {agent_text: s} replaced by its string, for display.
  function plain(v) {
    if (Array.isArray(v)) return v.map(plain);
    if (v && typeof v === "object") {
      var keys = Object.keys(v);
      if (keys.length === 1 && keys[0] === "agent_text") return String(v.agent_text);
      var o = {};
      keys.forEach(function (k) { o[k] = plain(v[k]); });
      return o;
    }
    return v;
  }
  function str(v) { v = plain(v); return v === null || v === undefined ? "" : typeof v === "string" ? v : JSON.stringify(v); }

  function table(head, rows) {
    var t = make("table", "gc-table");
    var tr = make("tr");
    head.forEach(function (h) { tr.appendChild(make("th", null, h)); });
    t.appendChild(tr);
    rows.forEach(function (r) {
      var row = make("tr");
      r.forEach(function (c) { row.appendChild(make("td", null, str(c))); });
      t.appendChild(row);
    });
    return t;
  }

  function testBlock(t) {
    var b = make("div", "cf-sec");
    b.appendChild(make("h4", null, str(t.test_id) + " - " + str(t.title) + " (" + str(t.origin) + ", " + str(t.run_count) + " run(s))"));
    if (t.diff) b.appendChild(section("Difference", t.diff));
    if ((t.prompts || []).length) {
      b.appendChild(table(["list", "step", "kind", "prompt", "bench config", "timeout ms"], t.prompts.map(function (p) {
        return [p.list, p.step, p.kind === null || p.kind === undefined ? "(none)" : p.kind, p.text, p.bench_config, p.timeout_ms];
      })));
    }
    if ((t.energising || []).length) {
      b.appendChild(make("div", null, "Will be powered:"));
      b.appendChild(table(["list", "step", "target", "line sent"], t.energising.map(function (e) { return [e.list, e.step, e.target, e.line]; })));
    }
    if ((t.limits || []).length) {
      b.appendChild(table(["measurement", "unit", "lower", "upper", "expect", "U", "k", "w"], t.limits.map(function (l) {
        return [l.measurement, l.unit, l.lower, l.upper, l.expect, l.U, l.k, l.w];
      })));
    }
    return b;
  }

  function drawRun(data, result) {
    gcCard.textContent = "";
    var card = data.card || {};
    var c = make("div", "cf-card");
    var head = make("div", "cf-head");
    head.appendChild(make("span", "badge", "run"));
    head.appendChild(make("span", "cf-name", data.run_id));
    var goBtn = make("button", "btn btn-primary cf-confirm", "Go");
    goBtn.type = "button";
    var unconfirmed = card.unconfirmed || [];
    if (!data.nonce) { goBtn.disabled = true; }
    if (unconfirmed.length) { goBtn.disabled = true; }
    head.appendChild(goBtn);
    c.appendChild(head);
    c.appendChild(make("div", "placeholder-note", "hash " + data.run_hash + " - upper bound on duration " + str(card.duration_bound_ms) + " ms" +
      (card.stop_on_fail ? " - stops on the first failure" : "")));
    if (unconfirmed.length) {
      c.appendChild(make("div", "cf-err", "The go waits on " + unconfirmed.length + " unconfirmed input(s), confirm them in the list above and load the card again:"));
      c.appendChild(table(["kind", "name", "hash"], unconfirmed.map(function (u) { return [u.kind, u.name, u.hash]; })));
    } else if (!data.nonce) {
      c.appendChild(make("div", "cf-err", "Core issued no nonce: this is not the person's token."));
    }
    ["suite", "standard", "dut"].forEach(function (k) { if (card[k] !== undefined) c.appendChild(section(k, plain(card[k]))); });
    (card.benches || []).forEach(function (bn) { c.appendChild(section("Bench " + str(bn.name), bn.tests)); });
    (card.tests || []).forEach(function (t) { c.appendChild(testBlock(t)); });
    if (result) c.appendChild(make("div", result.ok ? "gc-ok" : "cf-err", result.text));
    goBtn.addEventListener("click", function () { giveGo(data, goBtn); });
    gcCard.appendChild(c);
  }

  async function loadRun(result) {
    var id = gcRun.value.trim();
    if (!id) { gcState.textContent = "Enter a run id."; return; }
    gcState.textContent = "Loading...";
    try {
      var resp = await fetch("/api/test-runs/" + encodeURIComponent(id) + "/card");
      if (!resp.ok) { gcCard.textContent = ""; gcState.textContent = "Core refused (" + resp.status + "): " + (await resp.text()); return; }
      var data = await resp.json();
      gcState.textContent = "";
      drawRun(data, result);
    } catch (e) {
      gcState.textContent = "Could not read Core: " + (e && e.message ? e.message : String(e));
    }
  }

  async function giveGo(data, goBtn) {
    if (gcBusy) return;
    gcBusy = true;
    goBtn.disabled = true;
    var result;
    try {
      var resp = await fetch("/api/test-runs/" + encodeURIComponent(data.run_id) + "/go", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ run_hash: data.run_hash, nonce: data.nonce }),
      });
      var text = await resp.text();
      result = resp.ok ? { ok: true, text: "Go given: " + text } : { ok: false, text: "Refused (" + resp.status + "): " + text };
    } catch (e) {
      result = { ok: false, text: "The go was not sent: " + (e && e.message ? e.message : String(e)) };
    }
    gcBusy = false;
    await loadRun(result);
  }

  gcLoad.addEventListener("click", function () { loadRun(null); });
  gcRun.addEventListener("keydown", function (ev) { if (ev.key === "Enter") loadRun(null); });

  if (document.querySelector('.tab-panel.active[data-tab="confirm"]')) load(false);
  window.addEventListener("hashchange", function () { if (location.hash.indexOf("#confirm") === 0) load(false); });
})();
