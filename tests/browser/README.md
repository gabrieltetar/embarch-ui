# The Live Study tab, driven in a real browser

`cargo test` in this crate renders nothing — there is no JS engine in it, which
is why `tests/element_ids.rs` exists as a *text* guard and says so in its own
header. That guard catches a dangling id. It cannot catch a card that renders
blank, a status that reads as its opposite, or a console line that never
arrives.

This pair does. `stub_core.py` stands in for embarch-core and serves the states
that are hard to produce on a bench; `drive.py` opens the tab in headless
Firefox through geckodriver and asserts what is on screen.

## Where the scripts look: two variables, no hardcoded addresses

Every `drive*.py` learns its geckodriver and UI through `tests/browser/addr.py`
(`ui/094`): `EMBARCH_GECKO_PORT` (default 4444) and `EMBARCH_UI_PORT` (default
4899, or the script's own where it starts the UI itself). Each is a bare port on
127.0.0.1. The defaults stay the owner's interactive ports; the older names
(`GECKO`, `UI`, `WEBDRIVER`, `ATLAS_UI_PORT`, `LINKS_UI_PORT`, `DESK_UI_PORT`)
make a script exit with an error, because a variable that is silently ignored
is how a script lands on someone else's browser.

**A fleet worker never runs a script with these unset.** Pick two free ports
(not 4444, 4899, 4891), start your own geckodriver and UI, and record their real
PIDs:

```sh
geckodriver --port 4601 & GPID=$!        # plain shell, not a subshell
EMBARCH_UI_CONFIG=/tmp/ui.toml EMBARCH_UI_PORT=4602 ./target/debug/embarch-ui & UPID=$!
ss -ltnp | grep -E ':(4601|4602)\b'      # confirm both are yours
EMBARCH_GECKO_PORT=4601 EMBARCH_UI_PORT=4602 python3 tests/browser/drive.py
kill $GPID $UPID
```

`tests/drive_addresses.rs` fails `cargo test` if a drive script skips `addr.py`
or hardcodes either address, and runs the helper itself (no browser) to show
the variables are read and the legacy names refused. The usage blocks below
show the default ports for the owner's interactive use.

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

`drive_time_chart_caps.py` (ui/086) checks that a capped sample strip and a live mark lane opened after the ring dropped marks each show a note, and an uncapped one shows none. It needs `stub_core.py`'s `CAPPED` and `LIVEBIG` studies (opened by id, not listed) and a freshly started UI.

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
release build. The drop targets are the canvas's connectors, drawn as SVG,
so `tests/element_ids.rs` cannot see any of it.

```sh
python3 tests/browser/drive_topology.py
```

It starts the UI with scratch desk/XDG dirs, a private temp repo and an unused desk port. The enrol dialog went with the old diagram in phase 4; the moved-role note a confirmed enrolment shows (decision 61) is `drive_bench_live.py`'s.

`drive_payload_mode.py` is the sixth: the Study Designer row editor's
"Payload as" toggle, which also needs neither the stub nor a project — one
`raw` row is enough. It drives a payload through both directions of the
bytes/text round trip and the non-UTF8 edge case going bytes -> text has to
survive without throwing.

```sh
python3 tests/browser/drive_payload_mode.py
```

`drive_chart_field.py` is the eighth: the payload-layout editor's live-chart
field picker (`embarch-study-designer` decision 52's author-time half). It
needs an open project — a scratch firmware repo with nothing in it yet is
enough, since `study-structs.toml` not existing is the ordinary starting
state — but no stub, no study and no hardware. It authors a layout, picks a
`chart_field` from the field it just typed (proving the picker is live, not a
snapshot of what the layout had when the dialog opened), saves, and checks the
choice round-trips into the layout chip, into `study-structs.toml` itself, and
back into the picker on reopen — and that clearing it back to "none" is saved
as cleared rather than left stale.

```sh
mkdir -p /tmp/scratch-fw/embarch
printf '[study_designer]\nfirmware_repo_path = "/tmp/scratch-fw"\n' > /tmp/ui.toml
EMBARCH_UI_CONFIG=/tmp/ui.toml EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
python3 tests/browser/drive_chart_field.py
```

`drive_target_name.py` is the tenth: a `ble_connect` row's "Device name" box,
which needs neither the stub nor a project — one row is enough. It types a
name one character at a time and checks focus survives every keystroke,
including the first — the bug was `onSdRowInput`'s `targetName` branch
re-rendering the whole table on the blank/non-blank transition, which fires
exactly on character one into an empty box, dropping focus and the caret.
Also checks the "— any device!" hint clears and reappears with the box,
patched in place the same way `updateSdDelayHints` already was.

```sh
python3 tests/browser/drive_target_name.py
```

`drive_fonts.py` is the eleventh, and the only one that is about every page
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

A sixth: the running study's `bds-status` tap and the `StructChartValue` frame
it pushes — `embarch-study-designer` decision 52's live half. A real bench
would need a `Struct`-decoded tap whose layout declares a `chart_field` and a
DUT actually producing that payload; the stub fakes one point instead, which
is enough to prove the frame reaches `Series`, gets labelled "stream · field"
rather than a bare tap name, and lands in its own data card.

It is **not** a second implementation of embarch-core and must not grow into
one: it serves fixed JSON, and every route it answers is one the Live Study tab
actually calls. Where a behaviour depends on what embarch-core really does, the
bench is the test.

`drive_atlas.py` drives the Atlas tab. It brings its own project: a throwaway
git repo, an `embarch/atlas/` holding one atlas pinned to that repo's commit,
whose `graph.json` is `tests/fixtures/atlas/graph.json` — the synthetic
two-board product from `embarch-atlas`'s own tests, never a real one — and a
hand-written one-page PDF for every source document. It starts its own
release binary (no Core: the tab makes no Core call) and checks that every
part is drawn as a symbol, a box shows pin names rather than numbers, a pin's
label carries the net and the firmware's name for it, selecting a part draws
its wires, and a net, a problem, a code citation and a page render open in
the inspector.

```sh
cargo build --release
geckodriver --port 4444 &
python3 tests/browser/drive_atlas.py
```

`drive_links.py` drives the links into the page that say what to show
(`embarch-ui` decision 58): `#desk?item=…`, `#live-study?study=…` and
`#topology?board=…`, each as the address a page is loaded with and as a
`hashchange` while it is already open, plus a plain `#tab` and the retired
ones, and a parameter that would be markup if anything put it there. It
brings its own stub Core (`stub_core.py`'s studies, plus two enrolled roles),
a scratch embarch-desk and a scratch firmware repo, and points the UI's
recent-projects list and data directory at scratch too.

```sh
cargo build && (cd ../embarch-desk && cargo build)
geckodriver --port 4444 &
python3 tests/browser/drive_links.py
```

`drive_race.py` and `drive_switch.py` are the Live Study tab's two staleness
guards (`embarch-ui` decision 60). `drive_race.py` delays the browser's own
`fetch` of one study record, opens a second study meanwhile, and checks only
the second is on screen; run it against a **freshly started** UI, because its
second half needs the stub's running study live in the UI's session.
`drive_switch.py <repoA> <repoB>` switches project and checks the saved-study
picker follows. Start the UI for it with `EMBARCH_DESK_PORT` set to a port
nothing listens on and `EMBARCH_DESK_CONFIG_DIR` and `XDG_DATA_HOME` at scratch
directories: the page proxies to the real desk and writes the recents list, and
a click on "Open project" there posts to both.

`drive_prompt.py` (ui/101) renders the operator-prompt modal (decision 69): `stub_core.py` serves a host study with a `pending_prompt` once `POST /_stub/prompt/start` is called (a control surface on the stub's own port, no token). Needs `EMBARCH_STUB_PORT` set to the stub's port (default 4901) as well as the two address variables.

`drive_bench.py` drives the Topology tab as the bench canvas fills it
(`assets/bench.js`, the bench editor's read-only phase): the bar, the panels
over the canvas (Esc, their button, a click outside; Esc over a dialog closes
the dialog first; focus moves in and back), and the canvas itself.
The bench routes are served through a proxy the script puts in front of the
real binary, from `tests/fixtures/bench/catalog.json` — the vocabulary and the
built-ins exactly as this build serves them, rewritten with
`EMBARCH_REGEN_FIXTURES=1 cargo test` when a template changes — and
`fixture.json`, the repo templates and benches; `src/bench/drive_fixture.rs`
holds both to the real types. The proxy answers `409` while the UI has no
project open, passes everything else (`/events` included) through, and puts an
error listener at the top of the page. It checks the first project opened
loads, every look is drawn (an unknown look as a board, a missing or
unreadable template as an outline that says which), hovering reveals free
connectors without moving a wired one, this machine's ports and their names,
config tabs and a selection that follows its cable, a glow a snapshot lights
and puts out without rebuilding the canvas (probes and serial ports, by
interface, a serial-less bridge by vid and pid when one port alone matches),
Core not answering or not listing its ports, Validate's verdicts under the
parts and the Report button that reopens them, the desk banner's review, the
Saved benches panel and the canvas agreeing, pan, zoom, fit, a huge bench,
odd ids, phone width, and a server with no bench routes. It uses four ports:
`EMBARCH_UI_PORT` and the three after it (desk, proxy, stub Core);
`EMBARCH_BENCH_SHOTS=<dir>` also saves screenshots. `drive_topology.py` opens
each card's panel before touching it.

```sh
cargo build --release
geckodriver --port 4444 &
python3 tests/browser/drive_bench.py
```

`drive_bench_edit.py` drives editing on the canvas (the bench editor's phase
3) **against the real bench routes, never fixtures**: it writes a scratch
firmware repo holding `embarch/topologies/` and `embarch/parts/`, points the
UI at it with `[study_designer] firmware_repo_path`, and checks the TOML on
disk after every Save. A stub Core serves the probes and serial ports the
tray lists, and a pass-through proxy puts the error listener at the top of
the page. It checks the parts bin (click and drag to place, the next free id,
`embarch:edit-part` from Edit and New part…, a re-read on
`embarch:parts-changed`, `previewTemplate` as a pure draw), a cable laid by
click-then-click and by drag with the server's verdicts drawn (a fit pulsing,
a refusal red with its sentence beside the pointer, the hub's new-USB slot),
delete and undo across configs, a shield stacked, refused by full headers and
lifted, a probe and three port binding forms from the tray, the inspector's
label, role and flip, the Checks panel and its findings drawn on the parts,
Save, the `409` after a hand edit and its Reload, the unsaved-changes prompt
on a bench switch, a tab switch, a project switch and a reload, a new bench
through `POST`, and the config "A" a first part-to-part cable creates.
Fix round 1 added: what the canvas draws while a cable is laid compared end
by end with a direct `POST /api/bench/candidates`; Validate's verdicts
staying on the part they were placed on through a role edit, a tray bind and
an unbind; a confirmed leave that does not happen (a repo path that will not
open, a Cancel on a new bench's name) keeping the edits; a save held in the
proxy for 1.5 s being waited for; an edit under a cable being laid ending it;
the `x-embarch-refusal: rename` 409 offering no Reload; and the part
editor's unsaved draft (a stand-in `window.embarchParts`) asked about.
Fix round 2 added: a second Validate pass with the same words restoring
the verdicts (the pass stamp), the leave's dialog drawn over the real part
editor, Esc yielding to an open panel, an edit that changes nothing ending
a cable, and a project switch taking the old bench off the canvas. The
final round added: a save's 409 that lands after a project switch it was
not asked about being dropped, the old repo's file errors and Validate's
verdicts not shown under the new repo, and two animation frames after a
config tab click before any pointer action (also in `drive_bench.py`). It
uses four ports: `EMBARCH_UI_PORT` and the three after it (proxy, stub Core,
an unused desk port); `EMBARCH_BENCH_SHOTS=<dir>` saves screenshots in both
themes.

`drive_bench_parts.py` drives the part editor (`assets/bench-parts.js`, the
bench editor's phase 3) against the **real binary** over a scratch firmware
repo — the bench routes are not faked, so every file it checks is one the
server wrote and every refusal it reads is compared with what the same request
answers outside the browser. A pass-through proxy in front of the UI only puts
an error listener at the top of the page, so an error while the editor loads
is caught. It opens the editor the way the canvas's parts bin does, with an
`embarch:edit-part` event, and checks: a new template's fields and connectors
offer exactly the served lists (a power role only on a power kind, functions
only on the hub's kind, an interface and the served line facts only where the
vocabulary says), number fields as text captioned with their served ranges and
a wrong one sent as typed for the server to refuse, add stopping at the served
connector cap, add, move and remove a row, a passthrough pair that follows a
renamed connector, the file the save writes, an edit's `PUT` naming the
version it read, a hand edit on disk refused `409` with Reload, a built-in id
and an unknown kind refused in the server's words, a built-in read-only with
Copy to this repo, a file in `errors` with a confirmed fresh start the server
will not create until the file is gone, `embarch:parts-changed` on every save
(one still out when its draft is discarded included), a changed draft that
Cancel, Esc, another part or a project switch never drops without asking
(`window.embarchParts.unsaved()` following it), Esc ordering against a panel
and Core's prompt, and phone width. The preview is the canvas's
`window.embarchBench.previewTemplate`, which must draw and follow an edit;
with the canvas's object taken away, the dialog says the canvas did not load.
It uses four ports: `EMBARCH_UI_PORT`, a desk and a Core nothing listens on at
+1 and +2, and the proxy the browser opens at +3; `EMBARCH_UI_BIN` picks the
binary and `EMBARCH_PARTS_SHOTS=<dir>` saves screenshots of the dialog in both
themes.

```sh
cargo build --release
geckodriver --port 4444 &
python3 tests/browser/drive_bench_edit.py
python3 tests/browser/drive_bench_parts.py
```

`drive_bench_live.py` drives making a bench live (the bench editor's phase 4,
browser side): the bar's live state — none, live here, another bench,
superseded by another repo (one live bench per machine), its config gone,
its file gone or unparseable, unreadable with Read again — Make live and
Save and make live (`write_through: false`), the live report's rows (applied,
failed with Core's text, not sent; a proposal for a role and one for a bench
board, each confirmed through the ordinary enrol with its fields verbatim
and the bench then declared again; a part not proposed; leftovers each with
its own Retract, one Core refuses), focus landing on the report and never on
a Confirm when it opens by itself, a stale report's buttons disabled,
write-through after a Save and Declare again (`write_through: true`, shown as
a bar indicator rather than an opened report, and "saved, not declared" when
it is refused), a make-live answer dropped after a project switch, config
tabs as a view change with the live wiring marked, a make-live `409`, and
Stop being live. It also holds the races: a read of the record during a
make-live or a write-through cancelling neither, a make-live applied with no
record written keeping its report, a confirmed mark riding only into its own
Confirm's re-apply, a Confirm declaring again when a Declare again replaced
its report during the enrol, Esc from a report the bar's indicator opened
giving focus back to it, a bench opened from elsewhere right after a project
switch waiting for the bench list, and an unreadable record (a `500`) said
as unknown with its reason. The bench routes are the real binary's on a scratch repo;
the live routes are answered by a stub in the proxy, in the shapes
embarch-doc's `embarch-ui/interfaces/topology.md` gives them, unless `EMBARCH_LIVE_REAL=1` passes them
through to the binary's own: then the report's rows are the real apply's
over the stub Core — the probe a proposal would replace named, a plain enrol
with no moved-role note, a canonical role's Retract reaching Core. Four
ports: `EMBARCH_UI_PORT` and the three after it (proxy, stub Core, an unused
desk port); `EMBARCH_BENCH_SHOTS=<dir>` saves the bar and the report in both
themes.

Phase 4 deleted the old diagram, so `drive_topology.py` checks what it did
on the canvas instead: role parts drawn with their role, a role part's board
type picked in the inspector from the served pickers (the combinations the
scan backs, a chip from Core's SoC table, the bench file written and Core
not), the build combination pinned from the inspector's scan-backed Build as
(decision 47, embarch/boards.toml only; a dev-bench part offered none), a probe dropped from the tray onto a
role part's usb-host connector (decision 43's three properties on the tray's
targets, through a rebuild of the canvas), Validate's verdicts on the role
parts, and Saved benches' Open showing a bench without declaring anything.
`drive_links.py` marks the board's part on the canvas for
`#topology?board=` — by role, board type or the board's served label — and
opens no panel over a part the catalog also lists.

```sh
cargo build --release
geckodriver --port 4444 &
python3 tests/browser/drive_bench_live.py
```

Phase 5 added config slots to the same stub: Make B live on the live bench
records B through `POST /api/bench/live/config` with no make-live and nothing
sent to Core, and its answer is dropped after a project switch like a
make-live's.

`drive_bench_configs.py` drives config slots (the bench editor's phase 5) with
every route the real binary's, over a stub Core that records what it is sent:
+ Config (the first free letter offered, a copy of the config on screen or
empty), a name refused in the dialog in the vocabulary's own sentence and a
name the bench has refused, undo and redo, Rename and Delete, Save writing
them; Make live, then Make <config> live on the live bench recording the
config and sending Core nothing; a save that renames the live config moving
the record to the new name before it declares the bench, and one that deletes
it leaving the bar saying the config is gone, Make <config> live the way out;
a bench board picking a board type from the project's catalog in the
inspector (an analyzer is offered none), proposed on the save's write-through
and confirmed with no role and its label as the name; a bench with no config
keeping the row; a file holding a name a host study cannot write a finding
before Save and refused on Save in the server's words; an added config
renamed, made live, and the rename undone and redone across saves carrying
the record each way with its `since` kept; Save and make <config> live with
unsaved edits; clearing a board's role keeping its board type. Three ports from `EMBARCH_UI_PORT` (the UI, the
stub Core at +2, an unused desk port at +3); `EMBARCH_BENCH_SHOTS=<dir>` saves
the config row and the add dialog in both themes.

`drive_prompt_bench.py` drives the operator prompt naming a bench config
(phase 5, decision 69's modal): `stub_core.py`'s prompting study started with
a `bench_config`, the UI's real bench and live routes on a scratch repo, and a
proxy that can slow `GET /api/bench/live` and refuse the record-only route.
The live bench having the config draws the change (the canvas's renderer) and
lists the cables to unplug and to plug by part and connector label, and
Confirm answers, then records the config live; lacking it offers the bench's
configs and the text alone, a pick drawn and recorded, kept for the next
prompt of the run; no live bench, another repo's or an unreadable record is
the text alone and one line why. A slow lookup never holds the answer, a
refused mark is said while the answer stands — also when Core's resolve frame
beats the answer's own reply (`stub_core.py`'s `answer_delay`) — a prompt
arriving while a mark is on its way shows at once with a refusal on its own
line, the run keeps the text alone as its choice, a project switch reads the
bench again, the already-wired case still records, a live config gone from
the bench draws every cable to plug, and a prompt naming no config shows no
bench section. Ports: `EMBARCH_UI_PORT`, the proxy at +1,
`stub_core.py` at +2; `EMBARCH_BENCH_SHOTS=<dir>` saves the change and the pick
in both themes.

```sh
cargo build --release
geckodriver --port 4444 &
python3 tests/browser/drive_bench_configs.py
python3 tests/browser/drive_prompt_bench.py
```
