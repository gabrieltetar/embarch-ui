//! Regression guard: nothing in `assets/index.html` / `assets/app.js`
//! declares the same element id twice, and nothing in `app.js` looks up an
//! id that is declared nowhere.
//!
//! This is a text guard over the served assets, not a rendered check — there
//! is no JS engine in `cargo test` (see `src/study_designer.rs`'s
//! `run_badge_counter_names_the_step_now_running` for the same shape of
//! guard already in this crate). It finds no live defect today; it exists
//! because a Rust test suite that never renders anything cannot see this
//! class of bug at all, which is exactly how one shipped:
//! `embarch-ui/decisions/wiring.md` decision 10a cites
//! `embarch-ui/decisions/trace-chart.md` decision 10, where the Load button
//! and the load table's body carried the same element id, so every summary
//! row rendered into the button, the table never once displayed, and every
//! Rust test still passed.
//!
//! **Id universe.** `app.js` both reads markup (`getElementById` and its
//! `sdEl`/`trEl`/`sigEl` wrappers below) and emits it — three ids
//! (`tr-gap`, `tr-cross`, `tr-delay`) exist only because `app.js` builds an
//! SVG `<pattern id="...">` string for the trace chart's fills, with no
//! matching declaration in `index.html`. Those count as "declared" exactly
//! like an `index.html` id: they're never looked up via `getElementById`
//! (referenced only as `url(#tr-gap)` etc.), so they can't cause a dangling
//! lookup, but they *can* collide with another id, so both sources feed the
//! same duplicate check. Drawing the line this way is what keeps this guard
//! from false-positiving on a dynamically created id and getting deleted by
//! the next person who hits that (a real risk named in this unit's task).
//!
//! **Wrappers.** `sdEl`, `trEl` and `sigEl` are one-line wrappers around
//! `document.getElementById` (three of them — the task that opened this file
//! named only `sdEl`/`trEl`; `sigEl` is the same shape and is included here
//! too). This parser resolves a *literal* string argument to any of the four
//! call forms, which covers the overwhelming majority of call sites. It does
//! **not** trace a variable argument back to its literal source, and misses
//! exactly 10 call sites this way today, all in the `sd-req-*` requirement
//! fields code path (`sdReqFields`, `sdApplyRequires`, `sdLoadBenchState`),
//! which look elements up through `f.any`/`f.input`/`f.live`,
//! `pair[0]`/`pair[1]` and `row.any`/`row.input`/`row.live` rather than a
//! literal id. That resolves to 4 distinct ids — `sd-req-bench-any`,
//! `sd-req-bench-live`, `sd-req-dut-any`, `sd-req-dut-live` — each of which
//! is also declared in `index.html` today, so this is a coverage gap rather
//! than a false pass over a live defect: if one of those four is ever
//! renamed on only one side, this guard will not catch it.

use std::collections::{HashMap, HashSet};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");

/// Every `id="..."` attribute value in `src`, in source order — used
/// against both `index.html` (real HTML) and `app.js` (the markup it
/// constructs as JS string literals).
///
/// Boundary-checked so `data-row-id="..."` and `data-target-uuid="..."`
/// (both contain the substring `id="`) are never mistaken for an `id`
/// attribute: the character immediately before the match must not be
/// alphanumeric, `-` or `_`.
fn declared_ids(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let pat = "id=\"";
    let mut i = 0;
    while let Some(rel) = src[i..].find(pat) {
        let match_pos = i + rel;
        let boundary_ok = match src[..match_pos].chars().last() {
            Some(c) => !(c.is_alphanumeric() || c == '-' || c == '_'),
            None => true,
        };
        let value_start = match_pos + pat.len();
        let Some(end_rel) = src[value_start..].find('"') else {
            break;
        };
        let value_end = value_start + end_rel;
        if boundary_ok {
            out.push(src[value_start..value_end].to_string());
        }
        i = value_end + 1;
    }
    out
}

/// Literal string arguments to `fn_name("...")` in `src`, in source order.
/// See the module doc for what this deliberately does not resolve.
fn literal_calls(src: &str, fn_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let pat = format!("{fn_name}(\"");
    let mut i = 0;
    while let Some(rel) = src[i..].find(pat.as_str()) {
        let match_pos = i + rel;
        let boundary_ok = match src[..match_pos].chars().last() {
            Some(c) => !(c.is_alphanumeric() || c == '_'),
            None => true,
        };
        let value_start = match_pos + pat.len();
        let Some(end_rel) = src[value_start..].find('"') else {
            break;
        };
        let value_end = value_start + end_rel;
        if boundary_ok {
            out.push(src[value_start..value_end].to_string());
        }
        i = value_end + 1;
    }
    out
}

#[test]
fn no_element_id_is_declared_twice() {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for id in declared_ids(INDEX_HTML).into_iter().chain(declared_ids(APP_JS)) {
        *counts.entry(id).or_insert(0) += 1;
    }
    // Canary: if this stops matching anything (e.g. the markup moves to
    // single-quoted attributes), fail loudly rather than passing on zero
    // ids found. index.html alone declares 142 today.
    assert!(
        counts.len() > 100,
        "expected on the order of index.html's 142 declared ids; found {} \
         distinct ids — the `id=\"...\"` parser likely stopped matching",
        counts.len()
    );
    let mut dupes: Vec<String> = counts
        .iter()
        .filter(|(_, &n)| n > 1)
        .map(|(id, n)| format!("`{id}` declared {n} times"))
        .collect();
    dupes.sort();
    assert!(
        dupes.is_empty(),
        "duplicate element id(s) across index.html and the markup app.js emits:\n{}",
        dupes.join("\n")
    );
}

#[test]
fn every_looked_up_element_id_is_declared_somewhere() {
    let declared: HashSet<String> = declared_ids(INDEX_HTML)
        .into_iter()
        .chain(declared_ids(APP_JS))
        .collect();

    let mut lookups: Vec<(String, &'static str)> = Vec::new();
    for fn_name in ["getElementById", "sdEl", "trEl", "sigEl"] {
        for id in literal_calls(APP_JS, fn_name) {
            lookups.push((id, fn_name));
        }
    }
    // Canary, same reasoning as above: app.js has well over 100 literal
    // lookup call sites today across the four call forms.
    assert!(
        lookups.len() > 100,
        "expected well over 100 element id lookups across getElementById/sdEl/trEl/sigEl; \
         found {} — the call-site parser likely stopped matching",
        lookups.len()
    );

    let mut seen = HashSet::new();
    let mut missing: Vec<String> = Vec::new();
    for (id, fn_name) in &lookups {
        if !declared.contains(id) && seen.insert(id.clone()) {
            missing.push(format!(
                "`{id}` looked up via {fn_name}(\"{id}\") but declared nowhere in index.html or app.js's own emitted markup"
            ));
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "dangling element id lookup(s):\n{}",
        missing.join("\n")
    );
}
