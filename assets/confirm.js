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
  if (document.querySelector('.tab-panel.active[data-tab="confirm"]')) load(false);
  window.addEventListener("hashchange", function () { if (location.hash.indexOf("#confirm") === 0) load(false); });
})();
