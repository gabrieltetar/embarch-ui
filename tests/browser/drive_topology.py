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
import json, os, subprocess, sys, threading, time, urllib.request
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
ENROLLED = [
    {"probe_serial": "000683001234", "role": "dev-bench", "name": "bench-esp32c5",
     "chip": "esp32c5", "hardware_id": "aa:bb", "confirmed_at_utc_ms": NOW_MS,
     "link_serial": None},
    {"probe_serial": "ZZZ999", "role": "sniffer", "chip": "nRF52840",
     "hardware_id": "cc:dd", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
]
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
        if path == "/signals":
            return self._text("", 204)
        if path == "/dev-bench/link":
            LINKED.append(body)
            return self._text("", 204)
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

    # --- the Roles table ---------------------------------------------------
    cells = script("return Array.from(document.querySelectorAll('#topology-table-body tr'))"
                   ".map(function(tr){return Array.from(tr.children).map(function(td)"
                   "{return td.textContent.trim()})});")
    check("every row carries the board, the status badge and the enrolled instant",
          all(len(r) == 7 for r in cells), cells)
    check("a role is shown as a role, not as the lowercase wire spelling",
          [r[0] for r in cells[:2]] == ["Dev bench", "DUT"], [r[0] for r in cells])
    check("the board's own name is its own column",
          cells[0][1].startswith("bench-esp32c5"), cells[0][1])
    check("a name with no catalog entry says so rather than rendering as resolved",
          "not in catalog" in script("return document.getElementById('topology-table-body').innerHTML;"))
    check("an enrolled role shows a real timestamp, an empty one a dash",
          cells[0][5] != "—" and cells[1][5] == "—", [cells[0][5], cells[1][5]])
    check("a board enrolled outside the pair is still a row, flagged as not a role",
          any(r[0].startswith("sniffer") and "not a role" in r[0] for r in cells),
          [r[0] for r in cells])

    # --- what the alert list became ----------------------------------------
    check("the Topology tab no longer carries an alert list",
          script("return !document.getElementById('topology-alerts-list');"))
    check("the Dashboard keeps its own alert cards",
          script("return !!document.getElementById('dashboard-alerts-list');"))

    # --- the diagram's boxes are titled by role ----------------------------
    labels = script("return Array.from(document.querySelectorAll('#topology-diagram "
                    "[data-enroll-role] text')).map(function(t){return t.textContent});")
    check("the box's title is the role and the board is underneath it",
          labels[0] == "Dev bench" and "bench-esp32c5" in labels[1], labels)
    check("an empty role still has a title",
          labels[2] == "DUT" and labels[3] == "not enrolled", labels)

    # --- click-to-assign, onto the label rather than the rect --------------
    script("document.querySelector('#probes-pool .probe-card[data-serial=\"ABC123\"]').click();")
    script("var g=document.querySelector('[data-enroll-role=\"dev-bench\"]');"
           "g.querySelector('text').dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.5)
    check("clicking the box's *label* opens the dialog, not just the rect",
          script("return document.getElementById('assign-dialog').style.display === 'block';"))
    check("the chip arrives pre-filled from the enrolment being replaced",
          script("return document.getElementById('assign-chip').value;") == "esp32c5")
    note = script("return document.getElementById('assign-replace-note').textContent;")
    check("the dialog names what it displaces",
          "esp32c5" in note and "000683001234" in note, note)

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
    check("an unenrolled role pre-fills nothing and displaces nothing",
          script("return document.getElementById('assign-chip').value === '' && "
                 "document.getElementById('assign-replace-note').style.display === 'none';"))

    script("document.getElementById('assign-chip').value='nRF54L15';"
           "document.getElementById('assign-board').value='__new__';"
           "document.getElementById('assign-board').dispatchEvent(new Event('change'));"
           "document.getElementById('assign-new-board').value='wearable-rev6';"
           "document.getElementById('assign-confirm').click();")
    time.sleep(1.5)
    check("the enrolment reached Core with the dropped probe's serial and the board's name",
          POSTED == [{"role": "dut", "chip": "nRF54L15", "probe_serial": "ABC123",
                      "name": "wearable-rev6"}], POSTED)

    # --- the board catalog --------------------------------------------------
    check("the catalog names the file it is",
          "boards.toml" in script("return document.getElementById('board-catalog-path').textContent;"))
    script("document.getElementById('board-add').click();")
    time.sleep(0.3)
    script("document.getElementById('board-name').value='client-nucleo';"
           "document.getElementById('board-chip').value='STM32F407VG';"
           "document.getElementById('board-build-target').value='nucleo_f407zg';"
           "document.getElementById('board-save').click();")
    time.sleep(1)
    check("a saved board lands in the catalog table",
          "client-nucleo" in script("return document.getElementById('board-catalog-body')"
                                        ".textContent;"))
    check("and in the file the card names",
          os.path.exists(REPO + "/embarch/boards.toml"))
    script("document.querySelector('#probes-pool .probe-card[data-serial=\"ABC123\"]').click();"
           "var g=document.querySelector('[data-enroll-role=\"dut\"]');"
           "g.querySelector('rect').dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    time.sleep(0.4)
    check("the enroll dialog picks a board from the catalog",
          "client-nucleo" in script("return document.getElementById('assign-board').innerHTML;"))
    script("document.getElementById('assign-cancel').click();")

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
    check("and proposes each enrolment rather than performing it",
          script("return document.querySelectorAll('[data-proposal-role]').length;") >= 1,
          applied[:400])
    before = len(POSTED)
    script("document.querySelector('[data-proposal-role=\"dut\"]').click();")
    time.sleep(1.2)
    check("confirming a proposal runs the ordinary enrolment",
          len(POSTED) == before + 1 and POSTED[-1]["role"] == "dut", POSTED[-1:])

    # --- retracting a role --------------------------------------------------
    script("window.confirm=function(){return true};"
           "document.querySelector('[data-unenroll-role=\"sniffer\"]').click();")
    time.sleep(1.2)
    check("the leftover role can be cleared, which nothing could do before",
          DELETED == ["sniffer"], DELETED)

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
