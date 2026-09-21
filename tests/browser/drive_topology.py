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
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

TOKEN = "stub-token"
CORE_PORT = int(os.environ.get("EMBARCH_STUB_PORT", "4903"))
UI_PORT = os.environ.get("EMBARCH_UI_PORT", "4897")
UI = "http://127.0.0.1:" + UI_PORT
BASE = "http://127.0.0.1:4444"

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
POSTED = []
DELETED = []
LINKED = []
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
            ENROLLED = [b for b in ENROLLED if b["role"] != board["role"]] + [board]
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
REPO = "/tmp/embarch-ui-topology-drive-repo"
subprocess.run(["rm", "-rf", REPO], check=False)
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

cfg = "/tmp/embarch-ui-topology-drive.toml"
with open(cfg, "w") as f:
    f.write('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n' % (CORE_PORT, TOKEN))
env = dict(os.environ, EMBARCH_UI_CONFIG=cfg, EMBARCH_UI_PORT=UI_PORT)
ui = subprocess.Popen(["./target/release/embarch-ui"], env=env,
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


try:
    # --- the tab is gone, and its address still lands ----------------------
    rq("POST", b + "/url", {"url": UI + "/#enroll"})
    script("window.__errs=[];window.addEventListener('error',function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)

    check("the Enroll tab is gone from the sidebar",
          script("return !document.querySelector('.nav-item[data-tab=\"enroll\"]');"))
    check("the sidebar is five tabs",
          script("return document.querySelectorAll('.nav-item').length;") == 5)
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
    check("and where it is under that — the parent, since the name is the line above",
          script("return document.getElementById('project-current-path').textContent;") == "/tmp")
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

    # --- the pool and the targets ------------------------------------------
    check("the probe pool renders inside the diagram card",
          script("return !!document.getElementById('probes-pool').closest('.card')"
                 ".querySelector('#topology-diagram');"))
    check("both attached probes are offered",
          script("return document.querySelectorAll('#probes-pool .probe-card').length;") == 2)
    check("both role boxes are drop targets",
          script("return Array.from(document.querySelectorAll('#topology-diagram "
                 "[data-enroll-role]')).map(function(g){return g.getAttribute('data-enroll-role')});")
          == ["dev-bench", "dut"])

    # --- the roles table is gone; the diagram carries it -------------------
    check("the roles table is folded into the diagram, not sitting beside it",
          script("return !document.getElementById('topology-table-body');"))
    check("the Topology tab no longer carries an alert list",
          script("return !document.getElementById('topology-alerts-list');"))
    check("the Dashboard keeps its own alert cards",
          script("return !!document.getElementById('dashboard-alerts-list');"))

    # --- the boxes: role title, board line, no chip, no status yet ---------
    labels = script("return Array.from(document.querySelectorAll('#topology-diagram "
                    "[data-enroll-role] text')).map(function(t){return t.textContent});")
    check("the box's title is the role and the board type is under it",
          labels[0] == "Dev bench" and labels[1] == "Espressif ESP32-C5-DevKitC", labels)
    check("a role with no board type says what to do rather than going blank",
          labels[2] == "DUT" and labels[3] == "pick a board type", labels)
    check("the chip is not on the diagram any more",
          not any("esp32c5\u0020" in t or t.strip() == "esp32c5" for t in labels), labels)
    check("no status is shown before Validate topology has run",
          not any("validated" in t or "●" in t for t in labels), labels)
    check("the board line is its own click target",
          script("return Array.from(document.querySelectorAll('#topology-diagram "
                 "[data-board-role]')).map(function(g){return g.getAttribute('data-board-role')});")
          == ["dev-bench", "dut"])

    # --- the board-type catalog, which the DUT picker reads ----------------
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

    # --- picking a board type on the box -----------------------------------
    script("document.querySelector('[data-board-role=\"dut\"] text')"
           ".dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.5)
    check("clicking the board line opens the picker rather than the enrol dialog",
          script("return document.getElementById('role-board-dialog').style.display === 'block' "
                 "&& document.getElementById('assign-dialog').style.display !== 'block';"))
    dut_options = script("return Array.from(document.getElementById('role-board-select').options)"
                         ".map(function(o){return o.value});")
    check("the DUT picker offers this project's board types",
          dut_options == ["nrf52840dk/nrf52840"], dut_options)
    script("document.getElementById('role-board-cancel').click();")
    time.sleep(0.3)

    script("document.querySelector('[data-board-role=\"dev-bench\"] text')"
           ".dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.4)
    bench_options = script("return Array.from(document.getElementById('role-board-select').options)"
                           ".map(function(o){return o.textContent});")
    check("the dev-bench picker is the suite's fixed supported list, not the catalog",
          bench_options == ["Nordic nRF54L15 DK — nRF54L15",
                            "Espressif ESP32-C5-DevKitC — esp32c5"], bench_options)
    check("a dev-bench board is offered no build combination — it is not a project board",
          script("return document.getElementById('role-board-combo-field').style.display "
                 "=== 'none';"))
    script("document.getElementById('role-board-select').value='nrf54l15dk/nrf54l15/cpuapp';"
           "document.getElementById('role-board-save').click();")
    time.sleep(1.2)
    check("picking a board type writes it with no probe opened",
          BOARD_WRITES == [{"role": "dev-bench", "board": "nrf54l15dk/nrf54l15/cpuapp",
                            "chip": "nRF54L15"}], BOARD_WRITES)

    # --- what a board type can be *built* as (the catalog's own columns) ----
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

    # --- picking a *real* combination for the DUT --------------------------
    script("document.querySelector('[data-board-role=\"dut\"] text')"
           ".dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.6)
    script("document.getElementById('role-board-select').value='plank';"
           "document.getElementById('role-board-select')"
           ".dispatchEvent(new Event('change'));")
    time.sleep(0.4)
    # The catalog row states no chip — Rescan seeds none, because the scan
    # reports west targets. It reports each target's SoC too, and that is
    # Core's table's input (decision 48), so the picker has a chip anyway.
    check("a board type whose row states no chip is still offered with one",
          script("return document.getElementById('role-board-select')"
                 ".selectedOptions[0].textContent;") == "plank — nRF54L15")
    check("and it came from Core's SoC table, not from a string in the browser",
          "nrf54l15" in RESOLVED, RESOLVED)
    combos = script("return Array.from(document.getElementById('role-board-combo').options)"
                    ".map(function(o){return o.textContent});")
    check("the picker offers the combinations the scan found, not revisions × variants",
          len(combos) == 3, combos)
    check("each one names its revision and its variant, and stops there (decision 49)",
          combos and combos[0] == "rev 1 · no variant", combos)
    check("the west qualifier is on the option's tooltip, not in the line",
          script("return document.getElementById('role-board-combo').options[0].title;")
          == "plank@1/nrf54l15/cpuapp")
    check("the combination Zephyr does not back is not offered",
          not any("rev 1 · variant ns" in c for c in combos), combos)
    script("document.getElementById('role-board-combo').value='2';"
           "document.getElementById('role-board-save').click();")
    time.sleep(1.4)
    # One gesture, two owners, and the split is the point: which board type
    # is in a role is Core's fact and reaches Core unchanged, while which
    # combination of it this repo builds is the project's and lands in the
    # project's own file. Core is never told about a revision.
    check("Core is told the board type and nothing about the combination",
          BOARD_WRITES[-1] == {"role": "dut", "board": "plank", "chip": "nRF54L15"},
          BOARD_WRITES[-1])
    with open(REPO + "/embarch/boards.toml") as f:
        catalog = f.read()
    plank = catalog.split('name = "plank"')[-1]
    check("and the combination is recorded on the board in the project's own file",
          'revision = "2"' in plank and 'variant = "ns"' in plank, plank)
    check("the list marks the combination a build would use now",
          script("return !!document.querySelector('#board-catalog-body .build-chip.is-pinned');"))
    # The other end of decision 48: a board type nothing can answer for is
    # refused *here*, with the one thing a human can do about it, rather than
    # reaching Core and coming back as a relayed `502 … 400 Bad Request`.
    status, text = ui_post_status("/api/topology/roles/dut/board",
                                  {"board": "nothing-here", "chip": ""})
    check("a board type with no chip anywhere is refused here, not by Core",
          status == 400 and "Board types" in text, (status, text))

    # --- click-to-assign, onto the label rather than the rect --------------
    script("document.querySelector('#probes-pool .probe-card[data-serial=\"ABC123\"]').click();")
    script("var g=document.querySelector('[data-enroll-role=\"dev-bench\"]');"
           "g.querySelector('text').dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.5)
    check("clicking the box's *title* opens the enrol dialog, not just the rect",
          script("return document.getElementById('assign-dialog').style.display === 'block';"))
    check("the chip is read out of the role's board type rather than typed",
          script("return document.getElementById('assign-chip').value;") == "nRF54L15" and
          script("return document.getElementById('assign-chip').readOnly;"))
    note = script("return document.getElementById('assign-replace-note').textContent;")
    check("the dialog names the probe it would replace",
          "000683001234" in note, note)

    script("document.getElementById('assign-cancel').click();")
    time.sleep(0.3)

    # --- drag onto an empty role -------------------------------------------
    drag = ("var dt=new DataTransfer(); dt.setData('text/plain','ABC123');"
            "var g=document.querySelector('[data-enroll-role=\"dut\"]');"
            "var r=g.querySelector('rect');"
            "r.dispatchEvent(new DragEvent('dragover',{bubbles:true,dataTransfer:dt}));")
    script(drag)
    check("a drag over a box highlights that box",
          script("return document.querySelector('[data-enroll-role=\"dut\"]')"
                 ".classList.contains('dragover');"))

    # The diagram is rebuilt on every 5 s poll. A highlight that does not
    # survive one is a box that goes dark under a stationary cursor.
    time.sleep(6)
    check("the highlight survives the snapshot that rebuilds the box under it",
          script("return document.querySelector('[data-enroll-role=\"dut\"]')"
                 ".classList.contains('dragover');"))

    script("var dt=new DataTransfer(); dt.setData('text/plain','ABC123');"
           "var r=document.querySelector('[data-enroll-role=\"dut\"] rect');"
           "r.dispatchEvent(new DragEvent('drop',{bubbles:true,dataTransfer:dt}));")
    time.sleep(0.5)
    check("dropping opens the dialog for that role",
          script("return document.getElementById('assign-role-label').textContent;") == "DUT")
    check("a role with no probe bound displaces nothing",
          script("return document.getElementById('assign-replace-note').style.display === 'none';"))

    script("document.getElementById('assign-board').value='nrf52840dk/nrf52840';"
           "document.getElementById('assign-board').dispatchEvent(new Event('change'));"
           "document.getElementById('assign-confirm').click();")
    time.sleep(1.5)
    check("the enrolment carries the dropped probe and the board type's own chip",
          POSTED == [{"role": "dut", "chip": "nRF52840_xxAA", "probe_serial": "ABC123",
                      "name": "nrf52840dk/nrf52840"}], POSTED)

    # --- Validate topology --------------------------------------------------
    script("document.getElementById('topo-validate').click();")
    time.sleep(2)
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

    labels = script("return Array.from(document.querySelectorAll('#topology-diagram "
                    "[data-enroll-role] text')).map(function(t){return t.textContent});")
    check("the verdict lands on the box, which was blank until this run",
          any("validated" in t for t in labels), labels)
    check("a role Core refused shows as failed on its own box",
          any("failed" in t for t in labels), labels)

    # --- saving and loading a bench ----------------------------------------
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

    # And a canonical role is retracted from its own picker, since it has
    # no row anywhere either.
    script("document.querySelector('[data-board-role=\"dut\"] text')"
           ".dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.4)
    check("a role in a box is retracted from the picker that sets it",
          script("return document.getElementById('role-board-retract').offsetParent !== null;"))
    script("document.getElementById('role-board-retract').click();")
    time.sleep(1.2)
    check("retracting a role reaches Core", DELETED == ["sniffer", "dut"], DELETED)

    check("no uncaught JS error anywhere in the run",
          script("return window.__errs;") == [], script("return window.__errs;"))
finally:
    try:
        urllib.request.urlopen(urllib.request.Request(b, method="DELETE"), timeout=10)
    except Exception:
        pass
    ui.terminate()
    srv.shutdown()

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
