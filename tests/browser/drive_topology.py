"""The Topology tab, in a real browser.

The fourth `drive_*.py`, and the one that covers the fold: the Enroll tab's
whole surface moved onto the Topology tab's own diagram (decision 43), where
the box that says a role is empty *is* the box a probe is dropped onto to
fill it. None of that is reachable from `cargo test` — the drop target is an
SVG `<g>` rebuilt on every snapshot, the listeners are delegated onto the
`<svg>`, and the dialog it opens pre-fills from live data.

Decision 44 added the rest of what it drives: a role is a fixed slot and a
board's *name* is a separate fact, the alert list became a Validate-topology
pass, a role can be retracted, and a bench can be saved to and loaded from
the open project. The project halves need a real firmware repo on disk, so
this makes one in a temp directory and opens it through the same route the
Study Designer's own panel posts to.

**Phase 4 deleted the old diagram** (embarch-bench-editor-proposal.md, "What is
deleted"): its role boxes, probe pool and enrol and board-type dialogs. What
they did is checked here against the canvas instead — the role parts drawn
with their role, a role part's board type picked in the inspector from the
same served pickers, a probe dropped from the tray onto a role part's
usb-host connector (decision 43's three properties, on the tray's drop
targets now), and Validate's verdicts landing on the role parts. Making a
bench live, and confirming what it proposes, is `drive_bench_live.py`'s.

It brings its own stub rather than extending `stub_core.py`, which serves an
empty bench on purpose for `drive.py`'s Live Study fixtures: what this needs
is the opposite — attached probes, one role already enrolled (so the replace
path is real), one enrolled under a role outside the canonical pair, and a
`POST /probes/enroll` that records what it was sent.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_topology.py
"""
import json, os, subprocess, sys, threading, time, urllib.error, urllib.request
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

TOKEN = "stub-token"
CORE_PORT = int(os.environ.get("EMBARCH_STUB_PORT", "4903"))
UI_PORT = addr.ui_port(4897)
UI = "http://127.0.0.1:" + UI_PORT
BASE = addr.gecko_url()

NOW_MS = 1758240000000
PROBES = [
    {"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": "000683001234"},
    {"identifier": "CMSIS-DAP", "vendor_id": 12259, "product_id": 4, "serial_number": "ABC123"},
]
# dev-bench is enrolled (the replace path), dut is not (the empty path), and
# `sniffer` is enrolled under a role outside the canonical pair — the row the
# retired Enroll tab's own table was the only place to see.
# dev-bench holds both halves; dut holds neither yet (the state a bench is
# in while it is being set up, and the one the old row shape could not
# represent at all); `sniffer` is a leftover role from before the
# vocabulary closed.
ENROLLED = [
    {"probe_serial": "000683001234", "role": "dev-bench",
     "name": "esp32c5_devkitc/esp32c5/hpcore",
     "chip": "esp32c5", "hardware_id": "aa:bb", "confirmed_at_utc_ms": NOW_MS,
     "link_serial": None},
    {"probe_serial": "ZZZ999", "role": "sniffer", "chip": "nRF52840",
     "hardware_id": "cc:dd", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
]
BOARD_WRITES = []
# One declared signal whose carrier is *not* in the port enumeration below —
# the failing route the validate pass has to report as a failure rather than
# as a route it could not check.
SIGNALS = [
    {"name": "outpost", "origin_role": "dut", "direction": "dut-to-host",
     "route": {"kind": "direct", "port_serial": "MISSING-BRIDGE"}},
]
SERIAL_PORTS = [
    {"port_name": "COM7", "detected_by": "enumerated", "vendor_id": None,
     "product_id": None, "serial_number": "SOME-OTHER", "product": None,
     "interface": None},
]
# `None` serves a 404, which Core's client reads as "no port resolved" (the
# all-empty bench below); the default is the guessed port.
DEV_BENCH_PORT = "COM16"
POSTED = []
DELETED = []
LINKED = []
# Every `POST /signals` body, so a declared route's exact shape is checkable.
SIGNAL_POSTS = []
# Every SoC the UI asked Core to resolve — the fact that a chip nobody typed
# came from Core's table rather than from a string in `app.js` (decision 48).
RESOLVED = []


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _text(self, msg, code=404):
        body = msg.encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._text("unauthorized", 401)
        path = urlparse(self.path).path
        if path == "/status":
            return self._json({"status": "ok", "probes": PROBES,
                               "study_designer_schema_version": 1})
        if path == "/probes/enrolled":
            return self._json(ENROLLED)
        if path == "/signals":
            return self._json(SIGNALS)
        if path == "/serial-ports":
            return self._json(SERIAL_PORTS)
        if path == "/alerts":
            return self._json([])
        if path == "/dev-bench/port":
            # Guessed, not determined: the state the lowest-interface
            # fallback gets wrong on a two-VCOM probe, which the pass has to
            # show as something to look at rather than as a pass.
            if DEV_BENCH_PORT is None:
                return self._text("no dev-bench", 404)
            return self._json({"port_name": "COM16", "detected_by": "segger-vid-match",
                               "vendor_id": 4966, "product_id": 257,
                               "serial_number": "000683001234", "product": "J-Link",
                               "interface": 0, "guessed_among": 2})
        if path == "/studies":
            return self._json([])
        if path == "/logs/recent":
            return self._json({"lines": []})
        return self._text("not found in the stub", 404)

    def do_POST(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._text("unauthorized", 401)
        path = urlparse(self.path).path
        length = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        if path == "/probes/enroll":
            POSTED.append(body)
            board = {"probe_serial": body.get("probe_serial") or "",
                     "role": body["role"], "name": body.get("name") or "",
                     "chip": body["chip"],
                     "hardware_id": "ee:ff", "confirmed_at_utc_ms": NOW_MS,
                     "link_serial": None}
            # Kept, the way Core keeps it: a role is unique, so this
            # displaces whatever held it. Without this the tab would go on
            # showing an empty role after a successful enrolment, and every
            # check downstream of one would be testing the wrong state.
            global ENROLLED
            # Core decision 87: say what moved. `displaced` is a different
            # board that held the role; `vacated` is the role this probe held
            # before (kept when a board type was declared for it).
            same_role = [b for b in ENROLLED if b["role"] == board["role"]]
            held = [b for b in ENROLLED if b["probe_serial"] == board["probe_serial"]
                    and b["role"] != board["role"]]
            ENROLLED = [b for b in ENROLLED if b not in same_role and b not in held] + [board]
            if same_role and same_role[0]["probe_serial"] != board["probe_serial"]:
                board["displaced"] = same_role[0]
            if held:
                board["vacated"] = held[0]
                board["vacated_kept"] = bool(held[0].get("name"))
            return self._json(board)
        if path == "/validate":
            # dev-bench passes; anything else is a live mismatch, so the
            # report has one of each and a reason to print verbatim.
            if body.get("role") == "dev-bench":
                return self._json({"ok": True, "role": "dev-bench",
                                   "probe_serial": "000683001234", "chip": "esp32c5",
                                   "hardware_id": "aa:bb", "confirmed_at_utc_ms": NOW_MS,
                                   "validated_at_utc_ms": NOW_MS + 1000})
            return self._json({"kind": "mismatch", "role": body.get("role"),
                               "probe_serial": "ABC123", "chip": "nRF54L15",
                               "recorded_hardware_id": "ee:ff",
                               "live_hardware_id": "99:88",
                               "reason": "hardware_id changed under role",
                               "fix_it_url": "http://127.0.0.1:8765/#topology"}, 409)
        if path == "/resolve-chip":
            # Core's own SoC → probe-rs target table (its decision 8),
            # stubbed down to the one SoC this fixture's tree declares. An
            # unknown SoC answers `404`, exactly as Core's does, which is
            # what makes the refusal below a real path rather than a mocked
            # one.
            chip = {"nrf54l15": "nRF54L15"}.get((body.get("soc") or "").lower())
            if chip is None:
                return self._text("no known probe-rs chip mapping", 404)
            RESOLVED.append(body.get("soc"))
            return self._json({"chip": chip})
        if path == "/signals":
            SIGNAL_POSTS.append(body)
            return self._text("", 204)
        if path == "/dev-bench/link":
            LINKED.append(body)
            return self._text("", 204)
        return self._text("not found in the stub", 404)

    def do_PUT(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._text("unauthorized", 401)
        path = urlparse(self.path).path
        length = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        if path.startswith("/probes/enrolled/") and path.endswith("/board"):
            role = path.split("/")[3]
            BOARD_WRITES.append({"role": role, **body})
            global ENROLLED
            row = next((b for b in ENROLLED if b["role"] == role), None)
            if row is None:
                row = {"probe_serial": None, "role": role, "name": "", "chip": "",
                       "hardware_id": None, "confirmed_at_utc_ms": None,
                       "link_port_serial": None, "link_port_interface": None}
                ENROLLED = ENROLLED + [row]
            row["name"] = body["board"]
            row["chip"] = body["chip"]
            return self._json(row)
        return self._text("not found in the stub", 404)

    def do_DELETE(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._text("unauthorized", 401)
        path = urlparse(self.path).path
        if path.startswith("/probes/enrolled/"):
            role = path.rsplit("/", 1)[1]
            DELETED.append(role)
            global ENROLLED
            ENROLLED = [b for b in ENROLLED if b["role"] != role]
            return self._text("", 204)
        return self._text("not found in the stub", 404)


def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]


fails = []


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


# Threading, not one connection at a time: the UI polls six routes at
# once behind keep-alive, and a single-threaded stub serves the first
# and holds the rest until it times out.
srv = ThreadingHTTPServer(("127.0.0.1", CORE_PORT), Handler)
threading.Thread(target=srv.serve_forever, daemon=True).start()

# A firmware repo for the project halves. `.git` alone is enough for the
# open-project check (`api_open_project`'s own rule), and the catalog and the
# topologies directory are created on the first save.
import tempfile
_REPO_PARENT = tempfile.mkdtemp(prefix="embarch-ui-topology-drive-")
REPO = _REPO_PARENT + "/embarch-ui-topology-drive-repo"
os.makedirs(REPO + "/.git", exist_ok=True)

# A real enough Zephyr tree for the *build menu*: the board catalog lists what
# each board type can be built as, and the DUT picker binds to one of those
# combinations rather than to a board name alone.
#
# The fixture is shaped around the case a cross product gets wrong. `plank`
# declares revisions 1 and 2 and one named variant, `ns`. Bare `cpuapp`
# builds at 1 (the default revision, implicitly backed) and at 2 (an overlay
# backs it); the `ns` variant is backed only at 2, because Zephyr does not
# extend the default-revision shortcut to a named variant. So revisions ×
# variants is four combinations and only **three** are real — which is what
# the picker must offer.
BOARD_DIR = REPO + "/boards/acme/plank"
os.makedirs(BOARD_DIR, exist_ok=True)
os.makedirs(REPO + "/app/blinky", exist_ok=True)
os.makedirs(REPO + "/embarch", exist_ok=True)
open(REPO + "/app/blinky/CMakeLists.txt", "w").close()
with open(BOARD_DIR + "/board.yml", "w") as f:
    f.write("board:\n"
            "  name: plank\n"
            "  socs:\n"
            "    - name: nrf54l15\n"
            "      variants:\n"
            "        - name: ns\n"
            "          cpucluster: cpuapp\n"
            "  revision:\n"
            '    default: "1"\n'
            "    revisions:\n"
            '      - name: "1"\n'
            '      - name: "2"\n')
for name in ["plank_nrf54l15_cpuapp.dts", "plank_nrf54l15_cpuapp_ns.dts",
             "plank_nrf54l15_cpuapp_2.overlay", "plank_nrf54l15_cpuapp_ns_2.overlay"]:
    open(BOARD_DIR + "/" + name, "w").close()
# `embarch/embarch.toml` in the repo is `embarch-api`'s own documented
# fallback and the one this UI resolves per call, so no environment variable
# is needed to make the scan available.
with open(REPO + "/embarch/embarch.toml", "w") as f:
    f.write('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n\n'
            "[[projects]]\n"
            'name = "plank"\n'
            'source_path = "%s"\n'
            'discovery = "zephyr-west"\n'
            'flash_format = "hex"\n'
            # Both are required of a `zephyr-west` project by
            # `embarch-api`'s own config validation, and neither is executed
            # by a target scan: the scan is a filesystem walk over
            # `boards/` and `app/`, which is exactly why this fixture needs
            # no toolchain.
            'west_binary = "west"\n'
            'build_dir_root = "%s/build"\n' % (CORE_PORT, TOKEN, REPO, REPO))

# The bench the canvas draws: this machine, the dev bench and the DUT, each a
# part holding its role, no probe bound yet (the tray is how one is).
os.makedirs(REPO + "/embarch/topologies", exist_ok=True)
with open(REPO + "/embarch/topologies/topo.toml", "w") as f:
    f.write('name = "topo"\nsaved_at_utc_ms = 9\n'
            'parts = [{ id = "host", template = "this-machine", at = [20, 110] },'
            ' { id = "dev", template = "nrf54l15dk", role = "dev-bench", board = "esp32c5_devkitc/esp32c5/hpcore", at = [380, 110] },'
            ' { id = "dut", template = "generic-board", role = "dut", at = [380, 420] }]\n'
            'cables = [{ from = "dev.usb", to = "host" }]\n')

cfg = _REPO_PARENT + "/ui.toml"
with open(cfg, "w") as f:
    f.write('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n' % (CORE_PORT, TOKEN))
# Scratch desk config/data and a desk port nothing listens on: the page
# proxies to the desk and writes the recents list, and the owner's own desk
# service lives on :4891.
import tempfile
_iso = tempfile.mkdtemp(prefix="embarch-ui-topology-iso-")
env = dict(os.environ, EMBARCH_UI_CONFIG=cfg, EMBARCH_UI_PORT=UI_PORT,
           EMBARCH_DESK_PORT=str(int(UI_PORT) + 1),
           EMBARCH_DESK_CONFIG_DIR=_iso + "/desk-cfg", XDG_DATA_HOME=_iso + "/data",
           XDG_CONFIG_HOME=_iso + "/config",
           # The recents list is written on every project open; naming it
           # explicitly is what keeps a run off the owner's real
           # ~/.local/share/embarch/ui/recent-projects.json (ui/096).
           EMBARCH_UI_STATE=_iso + "/recent-projects.json")
env["HOME"] = _iso + "/home"
ui = subprocess.Popen([os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)

# Opened through the same route the Study Designer's panel posts to, so
# nothing here depends on a config file naming a repo. Not through `rq`,
# which unwraps WebDriver's own `{"value": …}` envelope.
def ui_post(path, body):
    req = urllib.request.Request(
        UI + path, data=json.dumps(body).encode(), method="POST",
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode()


def ui_post_status(path, body):
    """The same POST, for the refusals — status and body rather than a raise."""
    try:
        return 200, ui_post(path, body)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


ui_post("/api/study-designer/project", {"path": REPO})

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
    "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid


def script(js, args=None):
    return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})


def wait_for(js, timeout=10.0, args=None):
    end = time.time() + timeout
    while time.time() < end:
        v = script(js, args)
        if v:
            return v
        time.sleep(0.2)
    return script(js, args)


# The Topology tab is the bench canvas with every card a panel over it (owner,
# 2026-11-01): open the card's panel before touching it, the way a human has to.
def panel(name):
    script("var b=document.querySelector('[data-topo-panel=\"'+arguments[0]+'\"]');"
           "if (!b.classList.contains('is-open')) b.click();", [name])
    time.sleep(0.1)
    if not script("var p=document.getElementById('topo-panel-'+arguments[0]); return !p.hidden && p.offsetParent !== null;", [name]):
        raise SystemExit("the %s panel did not open" % name)


try:
    # --- the tab is gone, and its address still lands ----------------------
    rq("POST", b + "/url", {"url": UI + "/#enroll"})
    script("window.__errs=[];window.addEventListener('error',function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)

    check("the Enroll tab is gone from the sidebar",
          script("return !document.querySelector('.nav-item[data-tab=\"enroll\"]');"))
    check("the sidebar is six tabs: desk, topology, study designer, live study, atlas, debug",
          script("return Array.from(document.querySelectorAll('.nav-item')).map("
                 "function(b){return b.getAttribute('data-tab')});")
          == ["desk", "topology", "study-designer", "live-study", "atlas", "debug"])
    check("#enroll lands on Topology rather than on the remembered tab",
          script("return document.querySelector('.tab-panel[data-tab=\"topology\"]')"
                 ".classList.contains('active');"))

    # --- the open project, at the foot of the sidebar ----------------------
    #
    # Shell furniture, not a tab's: every tab on this page is relative to one
    # firmware repo, and this is where it is named and changed from.
    check("the sidebar names the open project",
          script("return document.getElementById('project-current-name').textContent;")
          == "embarch-ui-topology-drive-repo")
    check("and where it is under that — no desk is open in this isolated run, so it says so, then the parent",
          script("return document.getElementById('project-current-path').textContent;")
          == "no desk \u00b7 " + "/" + "/".join(_REPO_PARENT.strip("/").split("/")[-2:]))
    script("document.getElementById('project-button').click();")
    time.sleep(0.4)
    check("the control opens the picker from any tab",
          script("return document.getElementById('project-dialog').style.display === 'block';"))
    check("which offers the recents the server remembers",
          script("return document.getElementById('project-recents').options.length;") >= 1)
    script("document.getElementById('project-cancel').click();")
    time.sleep(0.3)
    check("the Study Designer's own project card no longer carries a second picker",
          script("return !document.getElementById('sd-project-picker');"))

    # --- the old diagram is gone; the canvas is the picture -------------------
    check("the Old diagram panel and its button are gone",
          script("return !document.getElementById('topo-panel-diagram') && !document.querySelector('[data-topo-panel=\"diagram\"]');"))
    check("and with it the probe pool, the role boxes and their enrol and board-type dialogs",
          script("return !document.getElementById('probes-pool') && !document.getElementById('topology-diagram') &&"
                 " !document.querySelector('[data-enroll-role]') && !document.getElementById('assign-dialog') &&"
                 " !document.getElementById('role-board-dialog');"))
    roles = wait_for("return Array.from(document.querySelectorAll('#bench-svg [data-part] .bn-role text')).map(function(t){return t.textContent;});", 10)
    check("the canvas draws each role's part with its role on it", sorted(roles or []) == ["DEV-BENCH", "DUT"], roles)
    check("no verdict is drawn before Validate topology has run",
          not script("return document.querySelectorAll('#bench-svg [data-verdict]').length;"))

    # --- the roles table is gone; the canvas carries it --------------------
    check("the roles table is folded into the picture, not sitting beside it",
          script("return !document.getElementById('topology-table-body');"))
    check("the Topology tab no longer carries an alert list",
          script("return !document.getElementById('topology-alerts-list');"))
    check("the Dashboard and its alert cards are gone; the Desk replaced them",
          script("return !document.getElementById('dashboard-alerts-list') && "
                 "!!document.querySelector('.tab-panel[data-tab=desk]');"))

    # --- the tray: what the probe pool was ------------------------------------
    tray = wait_for("return Array.from(document.querySelectorAll('#bench-tray [data-tray]')).map(function(c){return c.getAttribute('data-tray');});", 12)
    check("the tray offers both attached probes, since no part binds either",
          sorted(t for t in (tray or []) if t.startswith("probe:")) == ["probe:000683001234", "probe:ABC123"], tray)

    # --- the board-type catalog, which the DUT picker reads ----------------
    panel("boards")
    check("the catalog names the file it is",
          "boards.toml" in script("return document.getElementById('board-catalog-path').textContent;"))
    script("document.getElementById('board-add').click();")
    time.sleep(0.3)
    script("document.getElementById('board-name').value='nrf52840dk/nrf52840';"
           "document.getElementById('board-chip').value='nRF52840_xxAA';"
           "document.getElementById('board-build-target').value='nrf52840dk/nrf52840';"
           "document.getElementById('board-save').click();")
    time.sleep(1.2)
    check("a saved board type lands in the catalog table",
          "nrf52840dk" in script("return document.getElementById('board-catalog-body')"
                                 ".textContent;"))
    check("and in the file the card names",
          os.path.exists(REPO + "/embarch/boards.toml"))

    # --- a role part's board type, in the inspector ----------------------------
    # The role boxes' picker is gone; the same served pickers fill the select
    # a role part's inspector carries, and it writes the bench file — Core is
    # told when the bench is made live, never by picking.
    def inspect(pid):
        script("var e=new PointerEvent('pointerdown',{bubbles:true,button:0}); document.querySelector('#bench-svg [data-part=\"'+arguments[0]+'\"] .bn-body').dispatchEvent(e);"
               "document.querySelector('#bench-svg').dispatchEvent(new PointerEvent('pointerup',{bubbles:true,button:0}));", [pid])

    def board_options(pid, texts=False):
        inspect(pid)
        return wait_for("var s=document.querySelector('#bench-inspector [data-ed=\"board\"]');"
                        "if (!s || s.disabled) return null;"
                        "return Array.from(s.options).map(function(o){return " + ("o.textContent" if texts else "o.value") + "});", 8)
    writes = len(BOARD_WRITES)
    dut_options = board_options("dut")
    check("the DUT part's board type is picked from this project's board types",
          dut_options == ["", "nrf52840dk/nrf52840"], dut_options)
    bench_options = board_options("dev", texts=True)
    check("the dev bench part's is the suite's fixed supported list, not the catalog",
          bench_options == ["none", "Nordic nRF54L15 DK — nRF54L15", "Espressif ESP32-C5-DevKitC — esp32c5"], bench_options)
    script("var s=document.querySelector('#bench-inspector [data-ed=\"board\"]'); s.value='nrf54l15dk/nrf54l15/cpuapp'; s.dispatchEvent(new Event('change',{bubbles:true}));")
    time.sleep(0.4)
    check("picking one writes the bench, not Core", len(BOARD_WRITES) == writes and
          script("return !document.getElementById('bench-dirty').hidden;"), BOARD_WRITES[writes:])
    script("document.getElementById('bench-save').click();")
    time.sleep(1.0)
    with open(REPO + "/embarch/topologies/topo.toml") as f:
        saved = f.read()
    check("and Save puts it in the bench file", 'board = "nrf54l15dk/nrf54l15/cpuapp"' in saved, saved)

    # --- what a board type can be *built* as (the catalog's own columns) ----
    panel("boards")
    #
    # The list used to carry the chip and the west target. Neither is what a
    # human reads a bench list for; what this repo can build each board as
    # is, and it is the same menu the DUT picker binds to.
    check("a board type the repo's scan does not have says so rather than going blank",
          "not in the scan" in script("return document.getElementById('board-catalog-body')"
                                      ".textContent;"))
    script("document.getElementById('board-rescan').click();")
    time.sleep(1.6)
    row = script("return Array.from(document.querySelectorAll('#board-catalog-body tr'))"
                 ".filter(function(r){return r.textContent.indexOf('plank')===0;})"
                 ".map(function(r){return Array.from(r.cells).map(function(c){"
                 "return c.textContent.trim()});})[0];")
    check("the scan's board type is in the catalog after a rescan", bool(row), row)
    check("its revisions are listed, from the repo's own scan",
          row and row[1].replace(" ", "") == "12", row)
    check("its variants are listed beside them",
          row and row[2] == "ns", row)
    check("the apps it is in the tree for are not a column (decision 49)",
          row and len(row) == 5 and "blinky" not in row, row)
    check("the chip and the west target are off the list, not out of the row",
          "nRF52840_xxAA" not in script("return document.getElementById('board-catalog-body')"
                                        ".textContent;"))

    # --- a *real* combination for the DUT --------------------------------------
    plank_opts = board_options("dut", texts=True)
    # The catalog row states no chip — Rescan seeds none, because the scan
    # reports west targets. It reports each target's SoC too, and that is
    # Core's table's input (decision 48), so the picker has a chip anyway.
    check("a board type whose row states no chip is still offered with one",
          "plank — nRF54L15" in (plank_opts or []), plank_opts)
    check("and it came from Core's SoC table, not from a string in the browser",
          "nrf54l15" in RESOLVED, RESOLVED)
    script("var s=document.querySelector('#bench-inspector [data-ed=\"board\"]'); s.value='plank'; s.dispatchEvent(new Event('change',{bubbles:true}));")
    combos = wait_for("var n=document.querySelector('#bench-inspector .bn-ed-note'); return n ? n.textContent : '';", 6)
    check("the inspector names the combinations the scan found, not revisions × variants",
          (combos or "").count("rev ") == 3, combos)
    check("each one by its revision and its variant (decision 49)", "rev 1 · no variant" in (combos or ""), combos)
    check("the combination Zephyr does not back is not offered", "rev 1 · variant ns" not in (combos or ""), combos)
    script("document.getElementById('bench-save').click();")
    time.sleep(0.8)
    # The role route still answers what the deleted picker sent: one gesture,
    # two owners — the board type to Core, the combination to the project's
    # own file, and Core never told about a revision.
    ui_post("/api/topology/roles/dut/board", {"board": "plank", "chip": "", "revision": "2", "variant": "ns"})
    check("the role route tells Core the board type and nothing about the combination",
          BOARD_WRITES[-1] == {"role": "dut", "board": "plank", "chip": "nRF54L15"},
          BOARD_WRITES[-1])
    with open(REPO + "/embarch/boards.toml") as f:
        catalog = f.read()
    plank = catalog.split('name = "plank"')[-1]
    check("and records the combination on the board in the project's own file",
          'revision = "2"' in plank and 'variant = "ns"' in plank, plank)
    panel("boards")
    script("document.getElementById('board-rescan').click();")   # re-reads the catalog the route just pinned
    wait_for("return !!document.querySelector('#board-catalog-body .build-chip.is-pinned');", 8)
    check("the list marks the combination a build would use now",
          script("return !!document.querySelector('#board-catalog-body .build-chip.is-pinned');"))
    # The other end of decision 48: a board type nothing can answer for is
    # refused *here*, with the one thing a human can do about it, rather than
    # reaching Core and coming back as a relayed `502 … 400 Bad Request`.
    status, text = ui_post_status("/api/topology/roles/dut/board",
                                  {"board": "nothing-here", "chip": ""})
    check("a board type with no chip anywhere is refused here, not by Core",
          status == 400 and "Board types" in text, (status, text))

    # --- a probe onto a role part: the tray ---------------------------------------
    # Decision 43's three properties, on the tray's drop targets now: the
    # listeners are the canvas's, the connector under the pointer is state
    # drawn again on every rebuild, and only a connector that takes the probe
    # allows the drop.
    key_close = "\ue00c"
    rq("POST", b + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": [{"type": "keyDown", "value": key_close}, {"type": "keyUp", "value": key_close}]}]})
    rq("DELETE", b + "/actions")
    DRAG = ("var src=document.querySelector('#bench-tray [data-tray=\"probe:ABC123\"]'); window.__dt = window.__dt || new DataTransfer();"
            "if (arguments[0] === 'start') { src.dispatchEvent(new DragEvent('dragstart',{bubbles:true,cancelable:true,dataTransfer:__dt})); return true; }"
            "var t=document.querySelector('#bench-svg [data-conn=\"dut.usb\"]') || document.querySelector('#bench-svg [data-part=\"dut\"] [data-conn]');"
            "if (!t) return 'no target';"
            "var r=t.getBoundingClientRect(), x=r.left+r.width/2, y=r.top+r.height/2, at=document.elementFromPoint(x,y);"
            "var ev=new DragEvent(arguments[0],{bubbles:true,cancelable:true,dataTransfer:__dt,clientX:x,clientY:y}); at.dispatchEvent(ev);"
            "return arguments[0] === 'dragover' ? ev.defaultPrevented : true;")
    script(DRAG, ["start"])
    target = script("var t=document.querySelector('#bench-svg [data-part=\"dut\"] .bn-tray-ok'); return t ? t.getAttribute('data-conn') : null;")
    check("dragging a probe from the tray reveals the DUT part's free usb-host connector as a target", target is not None, target)
    allowed = script(DRAG, ["dragover"])
    check("the drop is allowed over that connector", allowed is True, allowed)
    check("which is marked as the one under the pointer",
          script("return !!document.querySelector('#bench-svg [data-part=\"dut\"] .bn-tray-ok.bn-target');"))
    time.sleep(6)
    check("and stays marked through the snapshot that lands meanwhile",
          script("return !!document.querySelector('#bench-svg [data-part=\"dut\"] .bn-tray-ok.bn-target');"))
    gap = script("var s=document.getElementById('bench-svg').getBoundingClientRect(); var x=s.left+8, y=s.top+s.height-40, at=document.elementFromPoint(x,y);"
                 "var ev=new DragEvent('dragover',{bubbles:true,cancelable:true,dataTransfer:window.__dt,clientX:x,clientY:y}); at.dispatchEvent(ev); return ev.defaultPrevented;")
    check("bare canvas between the parts does not take the drop", gap is False, gap)
    before = len(POSTED)
    script(DRAG, ["dragover"])
    script(DRAG, ["drop"])
    script("document.querySelector('#bench-tray [data-tray=\"probe:ABC123\"]') && document.querySelector('#bench-tray [data-tray=\"probe:ABC123\"]').dispatchEvent(new DragEvent('dragend',{bubbles:true,dataTransfer:window.__dt}));")
    time.sleep(0.4)
    script("document.getElementById('bench-save').click();")
    time.sleep(1.0)
    with open(REPO + "/embarch/topologies/topo.toml") as f:
        saved = f.read()
    check("dropping writes the probe into the DUT part's binding, and Save puts it in the file",
          'probe = "ABC123"' in saved, saved)
    check("binding a probe enrols nothing: making the bench live proposes that, for a human to confirm",
          len(POSTED) == before, POSTED[before:])
    # The ordinary enrol route is what a confirmed proposal calls.
    ui_post("/api/enroll", {"role": "dut", "chip": "nRF52840_xxAA", "probe_serial": "ABC123", "name": "nrf52840dk/nrf52840"})
    check("the ordinary enrol carries the probe and the board type's own chip",
          POSTED[-1:] == [{"role": "dut", "chip": "nRF52840_xxAA", "probe_serial": "ABC123",
                           "name": "nrf52840dk/nrf52840"}], POSTED[-1:])
    time.sleep(2.5)   # the validate pass reads the poll loop's snapshot, which the enrol pokes

    # --- Validate topology --------------------------------------------------
    script("document.getElementById('topo-validate').click();")
    time.sleep(2)
    check("Validate topology opens its report as a panel over the canvas",
          script("return !document.getElementById('topo-panel-validate').hidden;"))
    report = script("return document.getElementById('topo-validate-report').textContent;")
    check("a passing role is reported as a pass, under the role's own label",
          "Role Dev bench" in report and "pass" in report, report[:200])
    check("a mismatching role fails and prints Core's own reason verbatim",
          "hardware_id changed under role" in report, report[:400])
    check("a guessed dev-bench port is a warning, not a pass",
          "guessed among 2" in report, report[:400])
    check("a signal whose carrier is not enumerated fails",
          "MISSING-BRIDGE" in report, report[:600])
    check("the leftover role is called out as one",
          "sniffer" in report, report[:600])
    # A dev-bench board type comes from the suite's list, so the catalog
    # check must not nag about it — found on the live bench, first pass
    # after a board type was declared there.
    check("a supported bench board is not reported as missing from the project catalog",
          "no board type of that name" not in report, report[-400:])

    pills = wait_for("var o={}; document.querySelectorAll('#bench-svg [data-verdict]').forEach(function(g){ o[g.getAttribute('data-verdict')] = g.textContent; }); return Object.keys(o).length >= 2 ? o : null;", 8)
    check("the verdict lands on the role's part on the canvas, which carried none until this run",
          bool(pills) and "validated" in pills.get("dev", ""), pills)
    check("a role Core refused shows as failed on its own part", bool(pills) and "failed" in pills.get("dut", ""), pills)

    # --- saving and loading a bench ----------------------------------------
    panel("saved")
    script("document.getElementById('topo-profile-save').click();")
    time.sleep(0.3)
    script("document.getElementById('topo-save-name').value='bench a';"
           "document.getElementById('topo-save-confirm').click();")
    time.sleep(1)
    check("the bench is saved into the project, under a slug of its name",
          os.path.exists(REPO + "/embarch/topologies/bench-a.toml"))
    check("and the picker offers it",
          "bench a" in script("return document.getElementById('topo-profile-picker').textContent;"))

    script("document.getElementById('topo-profile-load').click();")
    time.sleep(1.5)
    applied = script("return document.getElementById('topo-apply-report').textContent;")
    check("loading re-declares the signals straight away",
          "signal outpost" in applied and "applied" in applied, applied[:300])
    check("and the board types, which claim nothing about silicon",
          "board" in applied and BOARD_WRITES[-1]["role"] in ("dut", "dev-bench"),
          [applied[:300], BOARD_WRITES[-1:]])
    check("and proposes each enrolment rather than performing it",
          script("return document.querySelectorAll('[data-proposal-role]').length;") >= 1,
          applied[:400])
    before = len(POSTED)
    script("document.querySelector('[data-proposal-role=\"dut\"]').click();")
    time.sleep(1.2)
    check("confirming a proposal runs the ordinary enrolment",
          len(POSTED) == before + 1 and POSTED[-1]["role"] == "dut", POSTED[-1:])

    # --- retracting a role --------------------------------------------------
    #
    # A leftover role is in neither box, so its only control is on the
    # validate report — the line that found it.
    script("document.getElementById('topo-validate').click();")
    time.sleep(2)
    check("the leftover role's finding carries its own clear control",
          script("return !!document.querySelector('#topo-validate-report "
                 "[data-unenroll-role=\"sniffer\"]');"))
    script("window.confirm=function(){return true};"
           "document.querySelector('#topo-validate-report [data-unenroll-role=\"sniffer\"]')"
           ".click();")
    time.sleep(1.2)
    check("the leftover role can be cleared, which nothing could do before",
          DELETED == ["sniffer"], DELETED)

    # A canonical role is retracted from the live report's leftovers now, by
    # its own click (drive_bench_live.py), never from a picker on a box.

    # --- the summary never says "passed" over a warning or an empty bench ----
    #
    # State here: dut and dev-bench are enrolled and one role fails, so the
    # first report above is a failure. Strip the bench down so that only the
    # guessed dev-bench port is left to find, then strip the port too.
    ENROLLED, SIGNALS = [], []

    def summary_after_poll():
        time.sleep(7)  # the validate pass reads the poll loop's snapshot
        script("document.getElementById('topo-validate').click();")
        time.sleep(2)
        return script("var s=document.querySelector('#topo-validate-report .topo-check-summary');"
                      "return s?s.textContent:'';")

    warn_only = summary_after_poll()
    check("a bench whose only finding is a guessed port never reads as passed",
          "every check passed" not in warn_only and "to look at" in warn_only, warn_only)
    DEV_BENCH_PORT = None
    all_empty = summary_after_poll()
    check("a bench with nothing to check says so, not that every check passed",
          "every check passed" not in all_empty and "nothing to check yet" in all_empty, all_empty)

    # --- enroll refuses a board with no chip, at the route ---------------------
    before = len(POSTED)
    status, text = ui_post_status("/api/enroll", {"role": "dut", "chip": "", "name": "no-such-board",
                                                  "probe_serial": "ABC123"})
    check("POST /api/enroll with an unanswerable board is a 400 naming where to set the chip, "
          "and Core is never asked",
          status == 400 and "Board types" in text and len(POSTED) == before, (status, text, POSTED[before:]))

    # --- a direct route on a two-interface device declares its interface ----
    panel("signals")
    #
    # suite/052 (embarch-topology decision 46, amended): one device exposing
    # two ports under one serial is told apart by interface, so picking one of
    # them declares it, while a one-port device's route keeps its old shape.
    SERIAL_PORTS = SERIAL_PORTS + [
        {"port_name": "COM15", "detected_by": "enumerated", "vendor_id": None,
         "product_id": None, "serial_number": "DUAL-1", "product": None, "interface": 0},
        {"port_name": "COM17", "detected_by": "enumerated", "vendor_id": None,
         "product_id": None, "serial_number": "DUAL-1", "product": None, "interface": 2},
    ]
    time.sleep(7)  # the dialog's port list is the poll loop's snapshot

    def declare_on(value, name):
        before = len(SIGNAL_POSTS)
        script("document.getElementById('sig-declare').click();")
        time.sleep(0.5)
        script("document.getElementById('sig-name').value=arguments[0];"
               "document.getElementById('sig-route').value='direct';"
               "document.getElementById('sig-route').dispatchEvent(new Event('change'));"
               "document.getElementById('sig-port').value=arguments[1];"
               "document.getElementById('sig-save').click();", [name, value])
        time.sleep(1.2)
        return SIGNAL_POSTS[before:]

    script("document.getElementById('sig-declare').click();")
    time.sleep(0.5)
    labels = script("return Array.from(document.getElementById('sig-port').options)"
                    ".map(function(o){return o.textContent;});")
    check("a port sharing its serial with another is labelled with its interface",
          any("COM17" in t and "interface 2" in t for t in labels)
          and not any("COM7" in t and "interface" in t for t in labels), labels)
    script("document.getElementById('sig-cancel').click();")
    time.sleep(0.3)
    sent = declare_on("DUAL-1|2", "console")
    check("picking one of two ports under one serial declares its interface",
          len(sent) == 1 and sent[0]["route"] == {"kind": "direct", "port_serial": "DUAL-1",
                                                  "interface": 2}, sent)
    sent = declare_on("SOME-OTHER|", "probe-console")
    check("a port alone under its serial is declared without one, as before",
          len(sent) == 1 and sent[0]["route"] == {"kind": "direct", "port_serial": "SOME-OTHER"},
          sent)

    check("no uncaught JS error anywhere in the run",
          script("return window.__errs;") == [], script("return window.__errs;"))
finally:
    try:
        urllib.request.urlopen(urllib.request.Request(b, method="DELETE"), timeout=10)
    except Exception:
        pass
    ui.terminate()
    srv.shutdown()
    subprocess.run(["rm", "-rf", _REPO_PARENT, _iso], check=False)

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
