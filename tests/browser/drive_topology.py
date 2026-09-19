"""Enrolling on the Topology tab, in a real browser.

The fourth `drive_*.py`, and the one that covers the fold: the Enroll tab's
whole surface moved onto the Topology tab's own diagram (decision 43), where
the box that says a role is empty *is* the box a probe is dropped onto to
fill it. None of that is reachable from `cargo test` — the drop target is an
SVG `<g>` rebuilt on every snapshot, the listeners are delegated onto the
`<svg>`, and the dialog it opens pre-fills from live data.

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
    {"probe_serial": "000683001234", "role": "dev-bench", "chip": "esp32c5",
     "hardware_id": "aa:bb", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
    {"probe_serial": "ZZZ999", "role": "sniffer", "chip": "nRF52840",
     "hardware_id": "cc:dd", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
]
POSTED = []


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
        if path == "/alerts" or path == "/signals" or path == "/serial-ports":
            return self._json([])
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
                     "role": body["role"], "chip": body["chip"],
                     "hardware_id": "ee:ff", "confirmed_at_utc_ms": NOW_MS,
                     "link_serial": None}
            return self._json(board)
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

cfg = "/tmp/embarch-ui-topology-drive.toml"
with open(cfg, "w") as f:
    f.write('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n' % (CORE_PORT, TOKEN))
env = dict(os.environ, EMBARCH_UI_CONFIG=cfg, EMBARCH_UI_PORT=UI_PORT)
ui = subprocess.Popen(["./target/release/embarch-ui"], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)

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

    # --- the merged Boards table -------------------------------------------
    cells = script("return Array.from(document.querySelectorAll('#topology-table-body tr'))"
                   ".map(function(tr){return Array.from(tr.children).map(function(td)"
                   "{return td.textContent.trim()})});")
    check("every row carries both the status badge and the enrolled instant",
          all(len(r) == 5 for r in cells), cells)
    check("an enrolled role shows a real timestamp, an empty one a dash",
          cells[0][4] != "—" and cells[1][4] == "—", [cells[0][4], cells[1][4]])
    check("a board enrolled outside the canonical pair is still a row",
          any(r[0] == "sniffer" for r in cells), [r[0] for r in cells])

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
          script("return document.getElementById('assign-role-label').textContent;") == "dut")
    check("an unenrolled role pre-fills nothing and displaces nothing",
          script("return document.getElementById('assign-chip').value === '' && "
                 "document.getElementById('assign-replace-note').style.display === 'none';"))

    script("document.getElementById('assign-chip').value='nRF54L15';"
           "document.getElementById('assign-confirm').click();")
    time.sleep(1.5)
    check("the enrolment reached Core with the dropped probe's serial",
          POSTED == [{"role": "dut", "chip": "nRF54L15", "probe_serial": "ABC123"}], POSTED)

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
