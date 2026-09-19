# The Live Study tab, driven in a real browser

`cargo test` in this crate renders nothing — there is no JS engine in it, which
is why `tests/element_ids.rs` exists as a *text* guard and says so in its own
header. That guard catches a dangling id. It cannot catch a card that renders
blank, a status that reads as its opposite, or a console line that never
arrives.

This pair does. `stub_core.py` stands in for embarch-core and serves the states
that are hard to produce on a bench; `drive.py` opens the tab in headless
Firefox through geckodriver and asserts what is on screen.

```sh
cargo build --release
python3 tests/browser/stub_core.py 4901 &
geckodriver --port 4444 &
printf '[core]\nbase_url = "http://127.0.0.1:4901"\ntoken = "stub-token"\n' > /tmp/ui.toml
EMBARCH_UI_CONFIG=/tmp/ui.toml EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
python3 tests/browser/drive.py
```

`drive_live.py <study_id>` is the other half: it points at a **real** embarch-core
and opens one completed study, checking every card against data embarch-core
actually holds — the steps, both consoles, the trace chart and its load
repartition, the GATT table, a `Raw` tap's hex head, the captured-streams table
and the provenance grid. It found `embarch-core` decision 71 (a study the job
registry had forgotten had no reachable provenance or per-tap byte counts).

```sh
python3 tests/browser/drive_live.py <a completed study_id with an outpost tap>
```

`drive_build.py` is the third: the Study Designer's Build card and the Debug
tab's `builds` log source, which need neither the stub nor a study. It drives
the **unavailable** path on purpose — no project open, no `embarch-api`
config, nothing ever built — because that is the state a fresh checkout is in
and the one where a card has to say why instead of showing a dead toggle.

```sh
python3 tests/browser/drive_build.py
```

`drive_topology.py` is the fourth: enrolling a board on the Topology tab,
which absorbed the Enroll tab (`embarch-ui` decision 43). It brings its own
stub — `stub_core.py` serves an empty bench for the fixtures above, and this
one needs attached probes, a role already enrolled, and a `POST /probes/enroll`
that records what it was sent — so it needs nothing but geckodriver and a
release build. The drop target is an SVG `<g>` rebuilt on every snapshot, so
`tests/element_ids.rs` cannot see any of it.

```sh
python3 tests/browser/drive_topology.py
```

`drive_fonts.py` is the fifth, and the only one that is about every page
rather than one tab: IBM Plex is served out of this binary (`embarch-ui`
decision 42), and this checks in a real browser that the faces actually load,
that no request went to a font CDN, and that the shipped Plex Mono's advance
is the number `assets/app.js` sizes the trace gutter from. It found that
constant wrong by 4.5% the first time it ran. It needs nothing but the UI.

```sh
python3 tests/browser/drive_fonts.py
```

**Not wired into `cargo test`**, deliberately: it needs geckodriver, a release
build and three processes, and a test that cannot run on a checkout is worse
than a script somebody runs on purpose. Run it after any change to
`assets/app.js`, `assets/index.html`, `src/live_study.rs` or
`src/studies_api.rs` — and `drive_fonts.py` after any change to
`assets/style.css`'s `@font-face` block or `assets/fonts/`.

**What the stub is for.** Five of its fixtures are states a bench will not
produce on demand: a study embarch-core reports as `interrupted`, one whose
`events.json` this build cannot parse, a `lagged` frame, a console chunk that
ends mid-line, and a study with neither a trace nor any data. Each of those has
an invariant attached to it — an interrupted study is never rendered as
completed or failed, an unreadable one never as empty, `lagged` is never
swallowed, a partial line is never padded into a whole one, and a study with no
data gets no empty card. The stub exists so those five are tested at all.

It is **not** a second implementation of embarch-core and must not grow into
one: it serves fixed JSON, and every route it answers is one the Live Study tab
actually calls. Where a behaviour depends on what embarch-core really does, the
bench is the test.
