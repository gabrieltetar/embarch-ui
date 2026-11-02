"""Making a bench live on the Topology tab, in a real browser.

Phase 4 of embarch-bench-editor-proposal.md, browser side: the bar's live
state, Make live (and Save and make live), the live report — applied rows,
proposals each confirmed through the ordinary enrol, rows not proposed, and
what Core holds that the bench does not name, each with its own Retract —
write-through after a Save, after a Confirm and from Declare again, Stop
being live, and the states a record can be in: none, live here, live in
another bench, superseded by another repo (one live bench per machine),
its config gone, its file gone or unparseable, and unreadable (a 500, said
as unknown with its reason). And the races: a read during a write cancels
nothing, an answer with no record keeps its report, a confirmed mark rides
only into its own re-apply, a Confirm declares again even when its report
was replaced during the enrol.

**The bench routes are the real binary's**, on a scratch firmware repo, so a
Save is a real PUT and the version the browser makes live with is checked
against the file. **The live routes are stubbed in a proxy** in front of the
UI, answering in the shapes embarch-doc's `embarch-ui/interfaces/topology.md`
gives the live routes; everything else —
`/api/enroll` included, which reaches the stub Core below — passes through.

**EMBARCH_LIVE_REAL=1 passes the live routes through to the binary's own**
and runs a flow whose checks hold against the real apply over the stub Core:
the report's rows (the DUT's board applied, its probe proposed naming the
probe it would replace, a bench board proposed, a canonical role left over),
Confirm sending each proposal's fields verbatim and the bench declared
again, a plain enrol with no moved-role note, a role's Retract reaching
Core, write-through, Stop being live.

Ports: geckodriver at EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, the
proxy the browser opens at +1, the stub Core at +2 and a desk port nothing
listens on at +3. EMBARCH_BENCH_SHOTS=<dir> saves screenshots in both themes.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_bench_live.py
"""
import base64, http.client, json, os, re, shutil, subprocess, sys, tempfile, threading, time, tomllib, urllib.request
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

REPO = Path(__file__).resolve().parents[2]
TOKEN = "stub-token"
UI_PORT = addr.ui_port(4885)
BASE = addr.gecko_url()
PROXY_PORT = int(UI_PORT) + 1
CORE_PORT = int(UI_PORT) + 2
DESK_PORT = int(UI_PORT) + 3
PAGE = "http://127.0.0.1:%d" % PROXY_PORT
SHOTS = os.environ.get("EMBARCH_BENCH_SHOTS")
REAL = os.environ.get("EMBARCH_LIVE_REAL") == "1"
NOW_MS = 1758240000000

DUT_PROBE = "DUTPROBE01"
OLD_PROBE = "OLDPROBE99"
POD_PROBE = "PODPROBE07"
DEV_PROBE = "DEVPROBE55"
PROBES = [{"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": DUT_PROBE},
          {"identifier": "CMSIS-DAP", "vendor_id": 12259, "product_id": 4, "serial_number": POD_PROBE}]
# The DUT role is held by a board on another probe, so confirming the DUT's
# proposal displaces it — and Core says so (decision 61). The dev-bench role
# is held too, and this bench has no part holding it: a leftover.
ENROLLED = [{"probe_serial": OLD_PROBE, "role": "dut", "name": "old-board", "chip": "nRF54L15",
             "hardware_id": "11:22", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
            {"probe_serial": DEV_PROBE, "role": "dev-bench", "name": "nrf54l15dk/nrf54l15/cpuapp", "chip": "nRF54L15",
             "hardware_id": "33:44", "confirmed_at_utc_ms": NOW_MS, "link_serial": None}]
ENROLLS = []
CORE_WRITES = []

LIVE_BENCH = """name = "live-bench"
saved_at_utc_ms = 3

[[parts]]
id = "host"
template = "this-machine"
at = [20, 110]

[[parts]]
id = "dut"
template = "nrf54l15dk"
role = "dut"
board = "nrf54l15dk/nrf54l15/cpuapp"
at = [380, 110]
usb = { usb = { probe = "%s" } }

[[parts]]
id = "pod"
template = "nucleo-g0b1re"
label = "probe-pod"
board = "nrf54l15dk/nrf54l15/cpuapp"
at = [380, 420]
usb = { stlink = { probe = "%s" } }

[[cables]]
from = "dut.usb"
to = "host"

[[cables]]
from = "pod.stlink"
to = "host"

[[configs]]
name = "A"

[[configs]]
name = "B"
""" % (DUT_PROBE, POD_PROBE)

OTHER_BENCH = """name = "other-bench"
saved_at_utc_ms = 2
parts = [{ id = "host", template = "this-machine", at = [20, 110] }, { id = "spare", template = "generic-board", at = [380, 110] }]
"""


def report_for(slug, config):
    return {
        "live": {"slug": slug, "config": config, "since_utc_ms": NOW_MS, "missing": False, "config_missing": False},
        "applied": [
            {"what": "dut board nrf54l15dk/nrf54l15/cpuapp (nRF54L15)", "route": "PUT /probes/enrolled/dut/board", "ok": True,
             "text": "Core holds the board type in the DUT role"},
            {"what": "signal uart-log", "route": "POST /signals", "ok": False,
             "text": "Core refused: lease held by session 4 (study pd-sweep)"},
            {"what": "part template embarch/parts/odd.toml", "route": "(not sent)", "ok": False,
             "text": "the template would not load, so what it declares was not sent"},
        ],
        "proposals": [
            {"kind": "enroll", "role": "dut", "name": "nrf54l15dk/nrf54l15/cpuapp", "chip": "nRF54L15", "probe_serial": DUT_PROBE,
             "part": "dut", "text": "dut: bind probe %s — displaces old-board on probe %s" % (DUT_PROBE, OLD_PROBE)},
            {"kind": "enroll", "role": None, "name": "probe-pod", "chip": "nRF54L15", "probe_serial": POD_PROBE,
             "part": "pod", "text": "probe-pod: enrol as a bench board on probe %s" % POD_PROBE},
        ],
        "not_proposed": [{"part": "spare", "text": "spare: not proposed — no probe is bound on it"}],
        "leftovers": [
            {"kind": "signal", "id": "old-sig", "text": "signal old-sig is declared in Core and this bench does not name it"},
            {"kind": "role", "id": "dev-bench", "text": "the dev-bench role is enrolled and this bench has no part holding it"},
        ],
    }


STATE = {"live": None, "superseded": None, "leftovers": [], "refuse_live": None, "delay_live": 0.0, "fail_get": False,
         "refuse_retract": None, "no_record": False, "delay_retract": 0.0}
ENROLL_DELAY = [0.0]
LIVE_POSTS = []
RETRACTS = []
LIVE_DELETES = []


class Core(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype="application/json"):
        if not isinstance(body, bytes):
            body = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _ok(self):
        return self.headers.get("authorization") == "Bearer " + TOKEN

    def _body(self):
        return json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"{}")

    def do_GET(self):
        if not self._ok():
            return self._send(401, b"unauthorized", "text/plain")
        path = urlparse(self.path).path
        answers = {"/status": {"status": "ok", "probes": PROBES, "study_designer_schema_version": 1},
                   "/probes/enrolled": ENROLLED, "/signals": [], "/serial-ports": [], "/alerts": [],
                   "/studies": [], "/logs/recent": {"lines": []}, "/instruments": [], "/bootload/ports": None}
        if path in answers:
            return self._send(200, answers[path])
        return self._send(404, b"not found in the stub", "text/plain")

    def do_POST(self):
        if not self._ok():
            return self._send(401, b"unauthorized", "text/plain")
        path = urlparse(self.path).path
        body = self._body()
        if path == "/probes/enroll":
            global ENROLLED
            if ENROLL_DELAY[0]:
                time.sleep(ENROLL_DELAY[0])
            ENROLLS.append(body)
            board = {"probe_serial": body.get("probe_serial") or "", "role": body["role"], "name": body.get("name") or "",
                     "chip": body["chip"], "hardware_id": "ee:ff", "confirmed_at_utc_ms": NOW_MS, "link_serial": None}
            same = [b for b in ENROLLED if board["role"] and b["role"] == board["role"]]
            ENROLLED = [b for b in ENROLLED if b not in same and b["probe_serial"] != board["probe_serial"]] + [board]
            if same and same[0]["probe_serial"] != board["probe_serial"]:
                board["displaced"] = same[0]
            return self._send(200, board)
        CORE_WRITES.append(("POST", path))
        return self._send(404, b"not found in the stub", "text/plain")

    def do_PUT(self):
        if not self._ok():
            return self._send(401, b"unauthorized", "text/plain")
        path = urlparse(self.path).path
        body = self._body()
        CORE_WRITES.append(("PUT", path))
        m = re.match(r"^/probes/enrolled/([^/]+)/board$", path)
        if m:
            row = next((b for b in ENROLLED if b["role"] == m.group(1)), None)
            if row is None:
                row = {"probe_serial": None, "role": m.group(1), "name": "", "chip": "", "hardware_id": None,
                       "confirmed_at_utc_ms": None, "link_serial": None}
                ENROLLED.append(row)
            row["name"], row["chip"] = body.get("board", ""), body.get("chip", "")
            return self._send(200, row)
        return self._send(404, b"not found in the stub", "text/plain")

    def do_DELETE(self):
        if not self._ok():
            return self._send(401, b"unauthorized", "text/plain")
        global ENROLLED
        path = urlparse(self.path).path
        CORE_WRITES.append(("DELETE", path))
        m = re.match(r"^/probes/enrolled/([^/]+)$", path)
        if m:
            ENROLLED = [b for b in ENROLLED if b["role"] != m.group(1)]
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        return self._send(404, b"not found in the stub", "text/plain")


LISTEN = (b"<script>window.__errs=[];addEventListener('error',function(e){__errs.push(String(e.message))});"
          b"addEventListener('unhandledrejection',function(e){__errs.push('unhandled: '+String(e.reason&&e.reason.message||e.reason))});</script>")


def upstream_json(path):
    with urllib.request.urlopen("http://127.0.0.1:%s%s" % (UI_PORT, path), timeout=20) as r:
        return json.loads(r.read().decode())


def refusal(code, text):
    return (409, text.encode(), "text/plain; charset=utf-8", {"x-embarch-refusal": code})


class Proxy(BaseHTTPRequestHandler):
    """The live routes from the contract, stubbed; everything else to the UI."""
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype="application/json", headers=None):
        if not isinstance(body, bytes):
            body = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _live(self, data):
        """Answers a live route and returns True, or returns False for a path that is none."""
        path = urlparse(self.path).path
        if path == "/api/bench/live" and self.command == "GET":
            if STATE["fail_get"]:
                self._send(500, b"could not read embarch/topologies/live-bench.toml: permission denied", "text/plain")
            else:
                self._send(200, {"live": STATE["live"], "superseded": STATE["superseded"]})
            return True
        if path == "/api/bench/live" and self.command == "DELETE":
            LIVE_DELETES.append(True)
            STATE["live"] = None
            STATE["superseded"] = None
            self._send(204, b"", "text/plain")
            return True
        if path == "/api/bench/live/retract" and self.command == "POST":
            RETRACTS.append(data)
            if STATE["delay_retract"]:
                # The server runs one make-live or retract at a time: a
                # re-apply that started meanwhile ran first, and this answer
                # carries the newer list.
                time.sleep(STATE["delay_retract"])
            offered = [x for x in STATE["leftovers"] if x["kind"] == data.get("kind") and x["id"] == data.get("id")]
            if STATE["superseded"]:
                self._send(*refusal("superseded", "another repo holds this machine; nothing was retracted"))
            elif not offered:
                self._send(*refusal("not-offered", "not a leftover this bench's report offered"))
            elif data.get("id") == STATE["refuse_retract"]:
                self._send(200, {"ok": False, "text": "Core refused: the %s role is leased by session 4" % data.get("id"),
                                 "leftovers": STATE["leftovers"]})
            else:
                STATE["leftovers"] = [x for x in STATE["leftovers"] if x not in offered]
                self._send(200, {"ok": True, "text": "retracted", "leftovers": STATE["leftovers"]})
            return True
        m = re.match(r"^/api/bench/benches/([a-z0-9-]+)/live$", path)
        if m and self.command == "POST":
            slug = m.group(1)
            if STATE["delay_live"]:
                time.sleep(STATE["delay_live"])
            current = upstream_json("/api/bench/benches/" + slug).get("version")
            LIVE_POSTS.append({"slug": slug, "body": data, "file_version": current})
            rec = STATE["live"]
            if "write_through" not in data:
                self._send(400, b"write_through is required", "text/plain")
            elif STATE["refuse_live"] or data.get("version") != current:
                self._send(*refusal("version-changed", "embarch/topologies/%s.toml changed since it was read; reload it and save again" % slug))
            elif data["write_through"] and (not rec or rec["slug"] != slug):
                self._send(*refusal("not-live", "%s is not this repo's live bench" % slug))
            elif data["write_through"] and STATE["superseded"]:
                self._send(*refusal("superseded", "another repo holds this machine: %s" % STATE["superseded"]["slug"]))
            elif data["write_through"] and rec.get("config_missing"):
                self._send(*refusal("config-missing", "the live config %s is not in %s any more" % (rec["config"], slug)))
            elif data["write_through"] and data.get("config") != rec["config"]:
                self._send(*refusal("config-differs", "a write-through names the live config %s" % rec["config"]))
            elif STATE["no_record"]:
                # Applied, and the record of which bench is live not written.
                rep = report_for(slug, data.get("config"))
                rep["live"] = None
                rep["applied"].append({"what": "live record", "route": "(this UI)", "ok": False,
                                       "text": "applied, but the record of which bench is live was not written: disk full"})
                self._send(200, rep)
            else:
                rep = report_for(slug, data.get("config"))
                STATE["live"] = rep["live"]
                STATE["superseded"] = None
                STATE["leftovers"] = rep["leftovers"]
                self._send(200, rep)
            return True
        return False

    def _forward(self):
        path = urlparse(self.path).path
        live_route = path.startswith("/api/bench/live") or re.match(r"^/api/bench/benches/[^/]+/live$", path)
        length = int(self.headers.get("content-length") or 0)
        data = self.rfile.read(length) if length else None
        if live_route:
            body = json.loads(data or b"{}") if data else {}
            if not REAL:
                if not self._live(body):
                    self._send(404, b"no such live route in the drive's stub", "text/plain")
                return
            # The real routes answer; the drive only notes what was asked.
            m = re.match(r"^/api/bench/benches/([a-z0-9-]+)/live$", path)
            if m and self.command == "POST":
                LIVE_POSTS.append({"slug": m.group(1), "body": body,
                                   "file_version": upstream_json("/api/bench/benches/" + m.group(1)).get("version")})
            elif path == "/api/bench/live/retract":
                RETRACTS.append(body)
            elif path == "/api/bench/live" and self.command == "DELETE":
                LIVE_DELETES.append(True)
        up = http.client.HTTPConnection("127.0.0.1", int(UI_PORT), timeout=120)
        drop = {"connection", "keep-alive", "accept-encoding", "content-length"}
        if path == "/":
            drop.add("if-none-match")
        headers = {k: v for k, v in self.headers.items() if k.lower() not in drop}
        try:
            up.request(self.command, self.path, body=data, headers=headers)
            resp = up.getresponse()
        except OSError as e:
            return self._send(502, str(e).encode(), "text/plain")
        self.send_response(resp.status)
        stream = (resp.getheader("content-type") or "").startswith("text/event-stream")
        for k, v in resp.getheaders():
            if k.lower() not in ("transfer-encoding", "connection", "content-length", "keep-alive", "etag"):
                self.send_header(k, v)
        if stream:
            self.end_headers()
            try:
                while True:
                    chunk = resp.read1(65536)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
            except (OSError, http.client.HTTPException):
                pass
            return
        body = resp.read()
        if path == "/" and resp.status == 200:
            body = body.replace(b"<head>", b"<head>" + LISTEN, 1)
        if resp.status != 304:
            self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body and resp.status != 304:
            self.wfile.write(body)

    do_GET = do_POST = do_PUT = do_DELETE = _forward


def serve(port, handler):
    srv = ThreadingHTTPServer(("127.0.0.1", port), handler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]


fails = []


def check(label, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + label + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(label)


core = serve(CORE_PORT, Core)
proxy = serve(PROXY_PORT, Proxy)

iso = tempfile.mkdtemp(prefix="embarch-ui-bench-live-")
fw = Path(iso) / "fw"
(fw / ".git").mkdir(parents=True)
TOPO = fw / "embarch" / "topologies"
TOPO.mkdir(parents=True)
(TOPO / "live-bench.toml").write_text(LIVE_BENCH)
(TOPO / "other-bench.toml").write_text(OTHER_BENCH)
# The DUT's board type is a catalog row with its chip, so the real apply can
# set it and propose the probe bound on the DUT (decision 48: looked up).
(fw / "embarch" / "boards.toml").write_text('[[boards]]\nname = "nrf54l15dk/nrf54l15/cpuapp"\nchip = "nRF54L15"\n'
                                            'build_target = "nrf54l15dk/nrf54l15/cpuapp"\n')
cfg = Path(iso) / "ui.toml"
cfg.write_text('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n\n[study_designer]\nfirmware_repo_path = "%s"\n'
               % (CORE_PORT, TOKEN, fw))
env = dict(os.environ, EMBARCH_UI_CONFIG=str(cfg), EMBARCH_UI_PORT=UI_PORT, EMBARCH_DESK_PORT=str(DESK_PORT),
           EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data", XDG_CONFIG_HOME=iso + "/config",
           EMBARCH_UI_STATE=iso + "/recent-projects.json", HOME=iso + "/home")
ui = subprocess.Popen([os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid


def script(js, args=None):
    return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})


def wait_for(js, timeout=10.0, args=None):
    end = time.time() + timeout
    while time.time() < end:
        v = script(js, args)
        if v:
            return v
        time.sleep(0.15)
    return script(js, args)


def until(cond, timeout=10.0):
    end = time.time() + timeout
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.15)
    return cond()


def element(css):
    return rq("POST", b + "/element", {"using": "css selector", "value": css})


def click(css, x=0, y=0):
    rq("POST", b + "/actions", {"actions": [{"type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"}, "actions": [
        {"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y},
        {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]})
    rq("DELETE", b + "/actions")


def click_row(css):
    """A button in the report: brought into view in its scrolling panel first (a Space
    there scrolls it, and smoothly), so the click lands on it and nowhere else."""
    wait_for("var e=document.querySelector(arguments[0]); return !!e && !e.disabled;", 6, [css])
    script("var e=document.querySelector(arguments[0]); if (e) e.scrollIntoView({block: 'center', behavior: 'instant'});", [css])
    settle()
    click(css)


def settle():
    rq("POST", b + "/execute/async", {"script": "var done = arguments[arguments.length - 1];"
                                                " requestAnimationFrame(function(){ requestAnimationFrame(function(){ done(true); }); });", "args": []})


def key(value):
    rq("POST", b + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": [
        {"type": "keyDown", "value": value}, {"type": "keyUp", "value": value}]}]})
    rq("DELETE", b + "/actions")


ESC = ""


def shot(label):
    if not SHOTS:
        return None
    os.makedirs(SHOTS, exist_ok=True)
    time.sleep(0.5)
    path = os.path.join(SHOTS, label)
    with open(path, "wb") as fh:
        fh.write(base64.b64decode(rq("GET", b + "/screenshot")))
    shots.append(path)
    return path


def theme(name):
    script("document.documentElement.setAttribute('data-theme', arguments[0]);", [name])


def both_themes(stem):
    if SHOTS:
        theme("dark"); shot(stem + "-dark.png")
        theme("light"); shot(stem + "-light.png")
        theme("dark")


def open_page():
    rq("POST", b + "/url", {"url": "about:blank"})
    rq("POST", b + "/url", {"url": PAGE + "/#topology"})


def on_bench(slug):
    return wait_for("return document.getElementById('bench-path').textContent.indexOf(arguments[0] + '.toml') >= 0 &&"
                    " !!document.querySelector('#bench-svg [data-part]');", 8, [slug])


def pick_bench(slug):
    script("var s=document.getElementById('bench-pick'); s.value=arguments[0]; s.dispatchEvent(new Event('change'));", [slug])


def bar():
    return script("function t(id){var e=document.getElementById(id); return e.hidden ? null : e.textContent;}"
                  "return {state: t('bench-live'), cls: document.getElementById('bench-live').className, make: t('bench-make-live'),"
                  " stop: !document.getElementById('bench-stop-live').hidden, again: !document.getElementById('bench-declare-again').hidden,"
                  " retry: !document.getElementById('bench-live-retry').hidden, wt: t('bench-live-wt')};")


def bar_settled():
    return wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent;", 8)


def panel_open(name):
    return script("var p=document.getElementById('topo-panel-'+arguments[0]); return !!p && !p.hidden && p.offsetParent !== null;", [name])


def report_text():
    return script("return document.getElementById('bench-live-body').innerText;") or ""


def notice():
    return script("var n=document.getElementById('bench-notice'); return n.hidden ? '' : n.textContent;")


def dirty():
    return script("return !document.getElementById('bench-dirty').hidden;")


def flip(pid):
    click('#bench-svg [data-part="%s"] .bn-body' % pid, -30, 4)
    key("f")
    wait_for("return !document.getElementById('bench-dirty').hidden;", 4)


def file_version(slug):
    return upstream_json("/api/bench/benches/" + slug).get("version")


def confirm_buttons():
    return script("return Array.from(document.querySelectorAll('#bench-live-body [data-live-confirm]')).map(function(b){"
                  "return [b.getAttribute('data-live-confirm'), b.disabled];});")


def events(name):
    return script("return window.__ev[arguments[0]] || [];", [name])


def listen():
    script("window.__ev={}; ['embarch:live-changed','embarch:role-changed'].forEach(function(n){"
           " document.addEventListener(n, function(e){ (window.__ev[n] = window.__ev[n] || []).push(e.detail); }); });")


def real_flow():
    check("REAL: the bar reads the binary's live record: no bench is live", bar_settled() == "not live", bar())
    click("#bench-make-live")
    opened = wait_for("return !document.getElementById('topo-panel-live').hidden && document.getElementById('bench-live-body').innerText.indexOf('Applied') >= 0;", 15)
    check("REAL: Make live posts the config on screen, the version read and write_through false, and the report opens",
          bool(opened) and len(LIVE_POSTS) == 1 and LIVE_POSTS[0]["body"] == {"config": "A", "version": LIVE_POSTS[0]["file_version"], "write_through": False},
          LIVE_POSTS)
    txt = report_text()
    check("REAL: the DUT's board type is an applied row", "dut board" in txt.lower() and "applied" in txt, txt[:700])
    check("REAL: its probe is proposed, naming the probe it would replace", DUT_PROBE in txt and OLD_PROBE in txt, txt[:900])
    check("REAL: the probe pod is proposed as a bench board", POD_PROBE in txt and "bench board" in txt, txt[:900])
    check("REAL: the dev-bench role Core holds and this bench does not is left over, with its Retract",
          "dev-bench" in txt and script("return !!document.querySelector('#bench-live-body [data-live-retract]');"), txt[-700:])
    check("REAL: focus is on the report, never on a button in it", script(
        "var a=document.activeElement; return a && a.id === 'topo-panel-live';"), script("return document.activeElement.outerHTML.slice(0, 80);"))
    b1 = bar()
    check("REAL: the bar says this bench is live, wired as config A, with Declare again", b1["state"] == "live · A" and b1["stop"] and b1["again"], b1)
    check("REAL: the record agrees", upstream_json("/api/bench/live").get("live", {}).get("slug") == "live-bench")
    # Confirm the DUT, then the pod.
    btns = script("return Array.from(document.querySelectorAll('#bench-live-body .bench-live-row')).map(function(r){"
                  " var b=r.querySelector('[data-live-confirm]'); return b ? [b.getAttribute('data-live-confirm'), r.innerText] : null;}).filter(Boolean);")
    dut_i = next((i for i, t in btns if DUT_PROBE in t), None)
    pod_i = next((i for i, t in btns if POD_PROBE in t), None)
    if dut_i is not None:
        posts = len(LIVE_POSTS)
        click_row('#bench-live-body [data-live-confirm="%s"]' % dut_i)
        until(lambda: len(LIVE_POSTS) > posts, 15)
        wait_for("return document.getElementById('bench-live-body').innerText.indexOf('declared again') >= 0;", 10)
        check("REAL: Confirm enrols the DUT's probe with the board type as the name", ENROLLS[-1:] and ENROLLS[-1].get("probe_serial") == DUT_PROBE and
              ENROLLS[-1].get("name") == "nrf54l15dk/nrf54l15/cpuapp" and ENROLLS[-1].get("role") == "dut", ENROLLS)
        check("REAL: and declares the bench again, as a write-through", len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"].get("write_through") is True,
              LIVE_POSTS[-1:])
        check("REAL: the report says what the enrol moved", "displaced" in report_text() and OLD_PROBE in report_text(), report_text()[:500])
    else:
        check("REAL: the DUT's proposal has a Confirm", False, btns)
    btns = script("return Array.from(document.querySelectorAll('#bench-live-body .bench-live-row')).map(function(r){"
                  " var b=r.querySelector('[data-live-confirm]'); return b ? [b.getAttribute('data-live-confirm'), r.innerText] : null;}).filter(Boolean);")
    pod_i = next((i for i, t in btns if POD_PROBE in t), None)
    if pod_i is not None:
        posts = len(LIVE_POSTS)
        click_row('#bench-live-body [data-live-confirm="%s"]' % pod_i)
        until(lambda: len(LIVE_POSTS) > posts, 15)
        said = wait_for("var s=document.querySelector('#bench-live-body .topo-check-summary'); return s && s.innerText.indexOf('probe-pod') >= 0 ? s.innerText : '';", 10)
        check("REAL: a bench board is enrolled with no role and its label as the name", ENROLLS[-1:] and ENROLLS[-1].get("role") == "" and
              ENROLLS[-1].get("probe_serial") == POD_PROBE and ENROLLS[-1].get("name") == "probe-pod", ENROLLS[-1:])
        check("REAL: a plain enrol shows no moved-role note", bool(said) and "displaced" not in said and "vacated" not in said, said)
    else:
        check("REAL: the bench board's proposal has a Confirm", False, btns)
    # Retract the dev-bench role.
    lefts = script("return Array.from(document.querySelectorAll('#bench-live-body [data-live-retract]')).map(function(b){"
                   " return [b.getAttribute('data-live-retract'), b.closest('.bench-live-row').innerText];});")
    dev_i = next((i for i, t in lefts if "dev-bench" in t), None)
    if dev_i is not None:
        click_row('#bench-live-body [data-live-retract="%s"]' % dev_i)
        until(lambda: ("DELETE", "/probes/enrolled/dev-bench") in CORE_WRITES, 10)
        check("REAL: retracting the canonical role reaches Core", ("DELETE", "/probes/enrolled/dev-bench") in CORE_WRITES, CORE_WRITES)
        check("REAL: and only that one", RETRACTS == [{"kind": "role", "id": "dev-bench"}], RETRACTS)
    else:
        check("REAL: the dev-bench leftover has a Retract", False, lefts)
    key(ESC)
    posts = len(LIVE_POSTS)
    flip("spare" if script("return !!document.querySelector('#bench-svg [data-part=\"spare\"]');") else "pod")
    click("#bench-save")
    until(lambda: len(LIVE_POSTS) > posts, 15)
    check("REAL: saving the live bench declares it again with the new version, as a write-through",
          len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"]["version"] == file_version("live-bench") and LIVE_POSTS[-1]["body"]["write_through"] is True,
          LIVE_POSTS[-1:])
    # This Save's own write-through, not the text the Confirm's re-apply left:
    # its answer has landed when Declare again acts again and the report
    # says it was saved.
    landed = wait_for("return !document.getElementById('bench-declare-again').disabled &&"
                      " document.getElementById('bench-live-body').innerText.indexOf('Saved, and declared again') >= 0;", 15)
    ind = wait_for("var w=document.getElementById('bench-live-wt'); return !w.hidden ? w.textContent : '';", 8)
    check("REAL: the write-through says so on the bar, without opening the report",
          bool(landed) and (ind or "").startswith("declared") and not panel_open("live"), [landed, ind])
    # A live bench file this process cannot read is the binary's 500, said as
    # unknown with its reason — never as a state of the file.
    os.chmod(TOPO / "live-bench.toml", 0)
    try:
        # Read again's own handler, pressed while hidden: the record read once more, and nothing else.
        script("document.getElementById('bench-live-retry').click();")
        unk = wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent.indexOf('unknown: ') === 0 ? l.textContent : '';", 8)
        check("REAL: an unreadable live bench file is said as unknown with the binary's reason, with Read again",
              bool(unk) and bar()["retry"] and "missing" not in bar()["cls"], [unk, bar()])
    finally:
        os.chmod(TOPO / "live-bench.toml", 0o644)
    wait_for("return !document.getElementById('bench-live-retry').hidden;", 8)
    click("#bench-live-retry")
    wait_for("return document.getElementById('bench-live').textContent.indexOf('live') === 0;", 8)
    on_bench("live-bench")
    wait_for("return !document.getElementById('bench-stop-live').hidden && !document.getElementById('bench-stop-live').disabled;", 8)
    click("#bench-stop-live")
    stopped = wait_for("return document.getElementById('bench-live').textContent === 'not live';", 8)
    after = upstream_json("/api/bench/live")
    check("REAL: Stop being live clears the record", bool(stopped) and after.get("live") is None, [bar(), after, LIVE_DELETES])
    check("REAL: no uncaught JS error or unhandled rejection", script("return window.__errs;") == [], script("return window.__errs;"))


shots = []
try:
    rq("POST", b + "/window/rect", {"width": 1700, "height": 1000})
    open_page()
    on_bench("live-bench")
    listen()

    if REAL:
        real_flow()
    else:
        # --- the old diagram is gone -----------------------------------------------------
        gone = script("return {panel: !!document.getElementById('topo-panel-diagram'), button: !!document.querySelector('[data-topo-panel=\"diagram\"]'),"
                      " svg: !!document.getElementById('topology-diagram'), pool: !!document.getElementById('probes-pool'),"
                      " enrol: !!document.getElementById('assign-dialog'), picker: !!document.getElementById('role-board-dialog')};")
        check("the Old diagram panel, its button, its probe pool and its two dialogs are gone", not any(gone.values()), gone)

        # --- not live ---------------------------------------------------------------------
        check("the bar says no bench is live for this repo", bar_settled() == "not live", bar())
        check("and offers Make live, nothing else", bar()["make"] == "Make live" and not bar()["stop"] and not bar()["again"], bar())
        check("with no report yet, no Live report button", script("return document.getElementById('bench-live-btn').hidden;"))

        # --- make a saved bench live --------------------------------------------------------
        click("#bench-make-live")
        opened = wait_for("return !document.getElementById('topo-panel-live').hidden;", 6)
        check("Make live posts the bench as saved: the config on screen, the version it read, write_through false",
              len(LIVE_POSTS) == 1 and LIVE_POSTS[0]["body"] == {"config": "A", "version": LIVE_POSTS[0]["file_version"], "write_through": False}, LIVE_POSTS)
        check("and the report opens as a panel", bool(opened) and panel_open("live"))
        check("with focus on the report itself, never on a Confirm one Space would press", script(
            "var a=document.activeElement; return !!a && a.id === 'topo-panel-live' && a.tagName !== 'BUTTON';"),
            script("return document.activeElement.outerHTML.slice(0, 80);"))
        key(" ")
        check("so a Space there enrols nothing", ENROLLS == [], ENROLLS)
        txt = report_text()
        check("an applied row says applied, a failed one failed with Core's own text, one not sent says not sent",
              "applied" in txt and "failed" in txt and "lease held by session 4" in txt and "not sent" in txt, txt)
        check("each proposal is a row with Confirm, saying Confirm reads the probe's hardware ID",
              [c[0] for c in confirm_buttons()] == ["0", "1"] and "hardware ID" in txt, txt)
        check("the DUT's proposal names the probe it would replace", OLD_PROBE in txt)
        check("a part not proposed says why", "not proposed" in txt and "no probe is bound on it" in txt)
        check("each leftover carries its own Retract, and nothing was retracted",
              script("return document.querySelectorAll('#bench-live-body [data-live-retract]').length;") == 2 and RETRACTS == [], txt)
        b1 = bar()
        check("the bar says this bench is live, wired as config A, with Declare again and Stop being live",
              b1["state"] == "live · A" and "bench-live-here" in b1["cls"] and b1["stop"] and b1["again"] and b1["make"] is None, b1)
        check("the config tab that is the live wiring is marked", script(
            "var t=document.querySelector('#bench-configs [data-live-config]'); return t ? t.textContent : null;") == "A")
        check("embarch:live-changed says which bench and config", events("embarch:live-changed") == [{"slug": "live-bench", "config": "A"}],
              events("embarch:live-changed"))
        check("the DUT's board type applied, so its Validate verdict is dropped", {"role": "dut", "probe_serial": None} in events("embarch:role-changed"),
              events("embarch:role-changed"))
        both_themes("live-report")
        check("making live touched no Core route from the page itself", CORE_WRITES == [], CORE_WRITES)

        # --- Confirm, the DUT then a bench board ----------------------------------------------
        posts = len(LIVE_POSTS)
        click_row('#bench-live-body [data-live-confirm="0"]')
        done = wait_for("var t=document.getElementById('bench-live-body').innerText; return t.indexOf('declared again') >= 0 ? t : '';", 8)
        check("Confirm runs the ordinary enrol with the proposal's fields verbatim — for a role, its board type as the name (decision 45)",
              ENROLLS == [{"role": "dut", "chip": "nRF54L15", "probe_serial": DUT_PROBE, "name": "nrf54l15dk/nrf54l15/cpuapp"}], ENROLLS)
        check("then declares the live bench again as a write-through, never taking the machine",
              len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"] == {"config": "A", "version": file_version("live-bench"), "write_through": True},
              LIVE_POSTS[-1:])
        check("and the fresh report says who was enrolled and what the enrol moved", bool(done) and "Enrolled dut" in done and OLD_PROBE in done, done)
        check("the confirmed proposal keeps its enrolled mark, no Confirm, in the report that came back", [c[0] for c in confirm_buttons()] == ["1"],
              confirm_buttons())
        # The enrol is slow, and Declare again replaces the report meanwhile:
        # the Confirm still declares again when it lands (L2).
        ENROLL_DELAY[0] = 1.2
        posts = len(LIVE_POSTS)
        click_row('#bench-live-body [data-live-confirm="1"]')
        time.sleep(0.3)
        script("document.getElementById('bench-declare-again').click();")
        until(lambda: len(LIVE_POSTS) >= posts + 2, 10)
        ENROLL_DELAY[0] = 0.0
        if not panel_open("live"):
            click("#bench-live-btn")
        said = wait_for("var s=document.querySelector('#bench-live-body .topo-check-summary'); return s && s.innerText.indexOf('probe-pod') >= 0 ? s.innerText : '';", 8)
        check("a bench-board proposal enrols with no role and its label as the name",
              ENROLLS[-1] == {"role": "", "chip": "nRF54L15", "probe_serial": POD_PROBE, "name": "probe-pod"}, ENROLLS[-1])
        check("a Confirm whose report a Declare again replaced during the enrol still declares again when it lands",
              len(LIVE_POSTS) == posts + 2 and all(x["body"]["write_through"] is True for x in LIVE_POSTS[posts:]), LIVE_POSTS[posts:])
        check("a plain enrol shows no moved-role note", bool(said) and "displaced" not in said, said)
        check("a confirmed mark rides only into its own Confirm's re-apply: the DUT's Confirm is back, the pod's marked",
              [c[0] for c in confirm_buttons()] == ["0"] and "Enrolled probe-pod" in report_text(), confirm_buttons())

        # --- Retract ------------------------------------------------------------------------
        STATE["refuse_retract"] = "dev-bench"
        click_row('#bench-live-body [data-live-retract="0"]')
        wait_for("return document.querySelectorAll('#bench-live-body [data-live-retract]').length === 1;", 6)
        check("Retract sends only the leftover clicked", RETRACTS == [{"kind": "signal", "id": "old-sig"}], RETRACTS)
        check("and the list comes back from the answer, the signal gone", "old-sig" not in report_text() and "dev-bench" in report_text())
        click_row('#bench-live-body [data-live-retract="0"]')
        refused = wait_for("var t=document.getElementById('bench-live-body').innerText; return t.indexOf('not retracted') >= 0 ? t : '';", 6)
        check("a Retract Core refuses says why on its row, and the leftover stays", "leased by session 4" in (refused or "") and
              script("return document.querySelectorAll('#bench-live-body [data-live-retract]').length;") == 1, refused)
        STATE["refuse_retract"] = None

        # --- reopen without a new apply -------------------------------------------------------
        key(ESC)
        check("Esc closes the report", not panel_open("live"))
        posts = len(LIVE_POSTS)
        click("#bench-live-btn")
        check("the Live report button reopens it without making live again", panel_open("live") and len(LIVE_POSTS) == posts)
        key(ESC)

        # --- switching configs is a view change ---------------------------------------------------
        script("document.querySelector('#bench-configs [data-cfg=\"1\"]').click();")
        settle()
        b2 = bar()
        check("switching to config B on the live bench calls no route, and the A tab stays the live wiring",
              len(LIVE_POSTS) == posts and script("return document.querySelector('#bench-configs [data-live-config]').textContent;") == "A", b2)
        check("the bar offers to make B the live wiring instead, as its own click", b2["make"] == "Make B live", b2)
        script("document.querySelector('#bench-configs [data-cfg=\"0\"]').click();")
        settle()

        # --- a write on its way holds the report's buttons; a late Retract still lands ------------------
        click("#bench-live-btn")
        STATE["delay_live"] = 1.0
        posts = len(LIVE_POSTS)
        script("document.getElementById('bench-declare-again').click();")
        held = wait_for("var b=document.querySelectorAll('#bench-live-body [data-live-retract], #bench-live-body [data-live-confirm]');"
                        " return b.length > 0 && Array.from(b).every(function(x){return x.disabled;});", 3)
        check("while a re-apply is on its way, the report's Confirm and Retract wait", bool(held), confirm_buttons())
        until(lambda: len(LIVE_POSTS) > posts, 8)
        STATE["delay_live"] = 0.0
        freed = wait_for("var b=document.querySelectorAll('#bench-live-body [data-live-retract]');"
                         " return b.length === 2 && Array.from(b).every(function(x){return !x.disabled;});", 6)
        check("and act again once it lands, on the report it answered", bool(freed), report_text()[-400:])
        STATE["delay_retract"] = 1.0
        retracts = len(RETRACTS)
        click_row('#bench-live-body [data-live-retract="0"]')
        time.sleep(0.25)
        posts = len(LIVE_POSTS)
        script("document.getElementById('bench-declare-again').click();")
        until(lambda: len(LIVE_POSTS) > posts, 6)
        late = wait_for("var n=document.getElementById('bench-notice'); return !n.hidden && n.textContent.indexOf('Retracted signal old-sig') === 0 ? n.textContent : '';", 6)
        STATE["delay_retract"] = 0.0
        check("a Retract answered after a re-apply replaced its report still says what it did",
              bool(late) and RETRACTS[retracts:] == [{"kind": "signal", "id": "old-sig"}], [notice(), RETRACTS[retracts:]])
        check("and its list, the newer one, goes on the report on screen: the signal gone, no Retract left for it",
              "old-sig" not in report_text() and "dev-bench" in report_text() and
              script("return document.querySelectorAll('#bench-live-body [data-live-retract]').length;") == 1, report_text()[-400:])
        key(ESC)
        posts = len(LIVE_POSTS)

        # --- write-through after a Save, and Declare again ----------------------------------------
        flip("spare" if script("return !!document.querySelector('#bench-svg [data-part=\"spare\"]');") else "pod")
        roles_before = len(events("embarch:role-changed"))
        # A read of the record while the write-through is on its way (a
        # bench list reload) must not cancel it (M1).
        STATE["delay_live"] = 1.0
        click("#bench-save")
        time.sleep(0.4)
        script("document.dispatchEvent(new CustomEvent('embarch:benches-changed'));")
        until(lambda: len(LIVE_POSTS) > posts, 8)
        STATE["delay_live"] = 0.0
        ind = wait_for("var w=document.getElementById('bench-live-wt'); return !w.hidden ? w.textContent : '';", 6)
        settled_bar = wait_for("return !document.getElementById('bench-declare-again').disabled && !document.getElementById('bench-stop-live').disabled;", 6)
        check("a read during the write-through drops nothing: the indicator says what it declared, Declare again and Stop wait no more",
              bool(settled_bar) and (ind or "").startswith("declared"), [ind, bar()])
        check("saving the live bench declares it again, as a write-through with the version the save answered",
              len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"] == {"config": "A", "version": file_version("live-bench"), "write_through": True},
              LIVE_POSTS[-1:])
        check("without opening the report: the bar says what it declared, a click away", ind == "declared · 2 proposals · 2 failed" and not panel_open("live"), ind)
        check("a re-apply whose rows say what the last report said drops no Validate verdict", len(events("embarch:role-changed")) == roles_before,
              events("embarch:role-changed")[roles_before:])
        both_themes("live-writethrough")
        click("#bench-live-wt")
        check("the indicator opens the report, focus on the report", panel_open("live") and script(
            "return document.activeElement && document.activeElement.id === 'topo-panel-live';"))
        check("which says it was declared again after the save", "declared again" in report_text(), report_text()[:200])
        key(ESC)
        check("Esc gives focus back to the indicator that opened it", bool(wait_for(
            "return document.activeElement && document.activeElement.id === 'bench-live-wt';", 3)),
            script("return document.activeElement ? document.activeElement.outerHTML.slice(0, 80) : null;"))
        posts = len(LIVE_POSTS)
        click("#bench-declare-again")
        until(lambda: len(LIVE_POSTS) > posts, 8)
        check("Declare again re-applies without an edit, as a write-through", len(LIVE_POSTS) == posts + 1 and
              LIVE_POSTS[-1]["body"]["write_through"] is True and LIVE_POSTS[-1]["body"]["config"] == "A", LIVE_POSTS[-1:])

        # --- superseded: another repo took the machine ----------------------------------------
        STATE["superseded"] = {"repo": "/home/someone/other-fw", "slug": "pd-rig", "config": None}
        posts = len(LIVE_POSTS)
        flip("dut")
        click("#bench-save")
        nd = wait_for("var w=document.getElementById('bench-live-wt'); return !w.hidden && w.textContent === 'saved, not declared' ? w.title : '';", 8)
        check("a write-through refused superseded says saved, not declared — on the bar, and it stays", "superseded" in (nd or "") or "another repo" in (nd or ""), nd)
        check("never as Not made live, never the Reload dialog", "Not made live" not in notice() and
              script("return document.getElementById('bench-dialog').style.display !== 'block';"), notice())
        sup = wait_for("var l=document.getElementById('bench-live'); return l.textContent.indexOf('superseded') === 0 ? l.textContent : '';", 6)
        check("the page reads the record again: superseded by the other repo and its bench", sup == "superseded by other-fw · pd-rig", sup)
        b3 = bar()
        check("Make live is the take-back; Declare again is gone", b3["make"] == "Make live" and not b3["again"], b3)
        click("#bench-live-btn")
        check("the report it kept is no longer the live one: its buttons act no more",
              all(c[1] for c in confirm_buttons()) and script("return Array.from(document.querySelectorAll('#bench-live-body [data-live-retract]')).every(function(b){return b.disabled;});"),
              confirm_buttons())
        key(ESC)
        posts2 = len(LIVE_POSTS)
        flip("dut")
        click("#bench-save")
        wait_for("return document.getElementById('bench-dirty').hidden;", 6)
        time.sleep(0.6)
        check("while superseded, a save is not sent to Core at all, and says saved, not declared",
              len(LIVE_POSTS) == posts2 and "not declared" in notice(), [LIVE_POSTS[posts2:], notice()])
        both_themes("live-superseded")
        roles_before = len(events("embarch:role-changed"))
        STATE["delay_live"] = 1.0
        click("#bench-make-live")
        time.sleep(0.4)
        script("document.dispatchEvent(new CustomEvent('embarch:benches-changed'));")
        back = wait_for("return document.getElementById('bench-live').textContent === 'live · A';", 8)
        STATE["delay_live"] = 0.0
        check("a read during a make-live drops nothing: the report opens and the buttons wait no more",
              bool(back) and bool(wait_for("return !document.getElementById('topo-panel-live').hidden;", 4)) and
              bool(wait_for("return !document.getElementById('bench-declare-again').disabled && !document.getElementById('bench-stop-live').disabled;", 4)), bar())
        check("Make live takes the machine back, as the explicit click it is", bool(back) and LIVE_POSTS[-1]["body"]["write_through"] is False and
              STATE["superseded"] is None, LIVE_POSTS[-1:])
        check("and the saved-not-declared mark is gone", bar()["wt"] is None, bar())
        check("an explicit make-live drops the verdicts its applied rows touch, unchanged or not",
              {"role": "dut", "probe_serial": None} in events("embarch:role-changed")[roles_before:], events("embarch:role-changed")[roles_before:])
        key(ESC)

        # --- the live config is gone --------------------------------------------------------------
        STATE["live"] = dict(STATE["live"], config_missing=True)
        open_page()
        on_bench("live-bench")
        listen()
        cm = wait_for("var l=document.getElementById('bench-live'); return l.textContent.indexOf('gone') >= 0 ? l.textContent : '';", 6)
        b4 = bar()
        check("a record whose config is gone says so", cm == "live config A is gone", b4)
        check("and offers Make live with a config that is there, no Declare again", b4["make"] == "Make live" and not b4["again"] and b4["stop"], b4)
        STATE["live"] = dict(STATE["live"], config_missing=False)

        # --- a project switch while a make-live is on its way ----------------------------------------
        open_page()
        on_bench("live-bench")
        listen()
        bar_settled()
        script("document.querySelector('#bench-configs [data-cfg=\"1\"]').click();")
        settle()
        STATE["delay_live"] = 1.5
        before = len(events("embarch:live-changed"))
        click("#bench-make-live")
        time.sleep(0.2)
        script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: arguments[0]}}));", [str(fw)])
        time.sleep(2.5)
        STATE["delay_live"] = 0.0
        check("a make-live answer that lands after a project switch is dropped: no event, no report opened over the new repo",
              len(events("embarch:live-changed")) == before and not panel_open("live"), events("embarch:live-changed"))
        on_bench("live-bench")

        # --- a 409 on make live offers Reload ------------------------------------------------------
        pick_bench("other-bench")
        on_bench("other-bench")
        bar_settled()
        b5 = bar()
        check("another bench is not the live one, and says which is", (b5["state"] or "").startswith("live: live-bench") and b5["make"] == "Make live" and not b5["stop"], b5)
        STATE["refuse_live"] = True
        click("#bench-make-live")
        asked = wait_for("return document.getElementById('bench-dialog').style.display === 'block' ? document.getElementById('bench-dialog').innerText : '';", 6)
        check("a make-live refused because the file changed offers Reload, never a silent overwrite", "changed since it was read" in (asked or "") and
              "Reload" in asked, asked)
        script("document.getElementById('bench-dialog-cancel').click();")
        STATE["refuse_live"] = None

        # --- applied, and the record not written -----------------------------------------------------
        STATE["no_record"] = True
        click("#bench-make-live")
        kept = wait_for("var p=document.getElementById('topo-panel-live'); return !p.hidden && document.getElementById('bench-live-body').innerText.indexOf('disk full') >= 0;", 8)
        STATE["no_record"] = False
        check("a make-live applied with no record written keeps its whole report, open, the reason on its row",
              bool(kept) and "Applied" in report_text() and "Proposed" in report_text(), report_text()[:300])
        check("its buttons wait: it is no live bench's report", confirm_buttons() and all(c[1] for c in confirm_buttons()), confirm_buttons())
        check("the notice says it was declared and not recorded, and the bar keeps the record it read",
              "not recorded" in notice() and (bar()["state"] or "").startswith("live: live-bench"), [notice(), bar()])
        key(ESC)

        # --- a bench opened from elsewhere right after a project switch waits for the list --------------
        script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: arguments[0]}}));"
               "window.embarchBench.showBench('other-bench', 'The desk lab uses');", [str(fw)])
        opened = wait_for("var n=document.getElementById('bench-notice'); return !n.hidden && n.textContent.indexOf('Make live declares it') >= 0 ? n.textContent : '';", 8)
        check("showBench right after a project switch waits for the bench list, then opens the bench",
              bool(opened) and "does not have" not in (opened or "") and bool(on_bench("other-bench")), [opened, notice()])
        bar_settled()

        # --- save and make live -----------------------------------------------------------------
        flip("spare")
        b6 = bar()
        check("with unsaved edits the button says Save and make live", b6["make"] == "Save and make live", b6)
        before = len(LIVE_POSTS)
        click("#bench-make-live")
        made = wait_for("return document.getElementById('bench-live').className.indexOf('bench-live-here') >= 0;", 8)
        flipped = tomllib.loads((TOPO / "other-bench.toml").read_text())
        check("it saves first, then makes live with the version that save answered", bool(made) and not dirty() and
              next(p for p in flipped["parts"] if p["id"] == "spare").get("flip") is True and
              len(LIVE_POSTS) == before + 1 and LIVE_POSTS[-1]["slug"] == "other-bench" and LIVE_POSTS[-1]["body"]["version"] == file_version("other-bench") and
              LIVE_POSTS[-1]["body"]["write_through"] is False, LIVE_POSTS[-1:])
        check("a bench with no config is made live with none", LIVE_POSTS[-1]["body"]["config"] is None and bar()["state"] == "live", bar())
        both_themes("live-bar")
        key(ESC)

        # --- stop being live -------------------------------------------------------------------
        writes = list(CORE_WRITES)
        click("#bench-stop-live")
        stopped = wait_for("return document.getElementById('bench-live').textContent === 'not live';", 6)
        check("Stop being live clears the record and touches nothing in Core", bool(stopped) and LIVE_DELETES == [True] and CORE_WRITES == writes,
              [LIVE_DELETES, CORE_WRITES])
        check("embarch:live-changed says no bench is live", events("embarch:live-changed")[-1:] == [{"slug": None, "config": None}])

        # --- a record whose file is gone, or does not parse --------------------------------------
        for flag, words in (("missing", "its file is gone"), ("unparseable", "its file does not parse")):
            STATE["live"] = {"slug": "gone-bench", "config": None, "since_utc_ms": NOW_MS, "missing": flag == "missing",
                             "unparseable": flag == "unparseable", "config_missing": False}
            open_page()
            on_bench("other-bench")
            miss = wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent.indexOf('gone-bench') >= 0 ? l.textContent : '';", 6)
            check("a live record whose file %s says so, never guesses" % ("is gone" if flag == "missing" else "does not parse"),
                  words in (miss or "") and "bench-live-missing" in bar()["cls"], bar())
            check("and offers to clear it (%s)" % flag, bar()["stop"] and bar()["make"] == "Make live", bar())
        click("#bench-stop-live")
        check("which clears it", bool(wait_for("return document.getElementById('bench-live').textContent === 'not live';", 6)))

        # --- unreadable: say so, and offer to read again ------------------------------------------
        STATE["fail_get"] = True
        open_page()
        on_bench("other-bench")
        unk = wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent.indexOf('unknown: ') === 0 ? l.textContent : '';", 6)
        check("a record that could not be read (a 500) says unknown with the reason, with Read again — never a state of the file",
              "permission denied" in (unk or "") and bar()["retry"] and "missing" not in bar()["cls"] and not bar()["stop"], bar())
        STATE["fail_get"] = False
        click("#bench-live-retry")
        check("and Read again reads it", bool(wait_for("return document.getElementById('bench-live').textContent === 'not live';", 6)), bar())

    check("no uncaught JS error or unhandled rejection, load included", script("return window.__errs;") == [], script("return window.__errs;"))
    if shots:
        print("screenshots: " + " ".join(shots))
finally:
    try:
        urllib.request.urlopen(urllib.request.Request(b, method="DELETE"), timeout=10)
    except Exception:
        pass
    ui.terminate()
    proxy.shutdown()
    core.shutdown()
    shutil.rmtree(iso, ignore_errors=True)

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
