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

**Not wired into `cargo test`**, deliberately: it needs geckodriver, a release
build and three processes, and a test that cannot run on a checkout is worse
than a script somebody runs on purpose. Run it after any change to
`assets/app.js`, `assets/index.html`, `src/live_study.rs` or
`src/studies_api.rs`.

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
