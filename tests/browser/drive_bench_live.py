"""Making a bench live on the Topology tab, in a real browser.

Phase 4 of embarch-bench-editor-proposal.md, browser side: the bar's live
state, Make live (and Save and make live), the live report — applied rows,
proposals each confirmed through the ordinary enrol, rows not proposed, and
what Core holds that the bench does not name, each with its own Retract —
write-through after a Save of the live bench, Stop being live, a record whose
file is gone, and the config tab that is the live wiring.

**The bench routes are the real binary's**, on a scratch firmware repo, so a
Save is a real PUT and the version the browser makes live with is checked
against the file. **The live routes are stubbed in a proxy** in front of the
UI, answering the shapes `coord/p4-contract.md` gives, until the server's
own land; everything else — `/api/enroll` included, which reaches the stub
Core below — passes through to the real UI.

**EMBARCH_LIVE_REAL=1 passes the live routes through to the binary's own**
(once they exist) and runs a shorter flow whose checks hold against any
Core answer: make live, the report's sections, Confirm sending the
proposal's fields verbatim and the bench declared again, write-through,
Stop being live.

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
from urllib.parse import urlparse, unquote

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
PROBES = [{"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": DUT_PROBE}]
# The DUT role is held today by a board on another probe, so confirming the
# proposal moves it — and Core says so (decision 61).
ENROLLED = [{"probe_serial": OLD_PROBE, "role": "dut", "name": "old-board", "chip": "nRF54L15",
             "hardware_id": "11:22", "confirmed_at_utc_ms": NOW_MS, "link_serial": None}]
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
id = "probe"
template = "debug-probe"
at = [380, 420]

[[cables]]
from = "dut.usb"
to = "host"

[[configs]]
name = "A"

[[configs]]
name = "B"
""" % DUT_PROBE

OTHER_BENCH = """name = "other-bench"
saved_at_utc_ms = 2
parts = [{ id = "host", template = "this-machine", at = [20, 110] }, { id = "probe", template = "debug-probe", at = [380, 110] }]
"""


def report_for(slug, config):
    return {
        "live": {"slug": slug, "config": config, "since_utc_ms": NOW_MS, "missing": False},
        "applied": [
            {"what": "dut board nrf54l15dk/nrf54l15/cpuapp (nRF54L15)", "route": "PUT /probes/enrolled/dut/board", "ok": True,
             "text": "Core holds the board type in the DUT role"},
            {"what": "signal uart-log", "route": "POST /signals", "ok": False,
             "text": "Core refused: lease held by session 4 (study pd-sweep)"},
        ],
        "proposals": [
            {"kind": "enroll", "role": "dut", "name": "nrf54l15dk/nrf54l15/cpuapp", "chip": "nRF54L15", "probe_serial": DUT_PROBE,
             "part": "dut", "text": "dut: bind probe %s, reading the board's hardware ID" % DUT_PROBE},
        ],
        "not_proposed": [{"part": "probe", "text": "probe: not proposed — no probe is bound on it"}],
        "leftovers": [
            {"kind": "signal", "id": "old-sig", "text": "signal old-sig is declared in Core and this bench does not name it"},
            {"kind": "role", "id": "sniffer", "text": "the sniffer role is enrolled and this bench has no part holding it"},
        ],
    }


STATE = {"live": None, "leftovers": [], "refuse_live": None}
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

    def do_GET(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._send(401, b"unauthorized", "text/plain")
        path = urlparse(self.path).path
        answers = {"/status": {"status": "ok", "probes": PROBES, "study_designer_schema_version": 1},
                   "/probes/enrolled": ENROLLED, "/signals": [], "/serial-ports": [], "/alerts": [],
                   "/studies": [], "/logs/recent": {"lines": []}}
        if path in answers:
            return self._send(200, answers[path])
        return self._send(404, b"not found in the stub", "text/plain")

    def do_POST(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._send(401, b"unauthorized", "text/plain")
        path = urlparse(self.path).path
        body = json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"{}")
        if path == "/probes/enroll":
            global ENROLLED
            ENROLLS.append(body)
            board = {"probe_serial": body.get("probe_serial") or "", "role": body["role"], "name": body.get("name") or "",
                     "chip": body["chip"], "hardware_id": "ee:ff", "confirmed_at_utc_ms": NOW_MS, "link_serial": None}
            same = [b for b in ENROLLED if b["role"] == board["role"]]
            ENROLLED = [b for b in ENROLLED if b not in same] + [board]
            if same and same[0]["probe_serial"] != board["probe_serial"]:
                board["displaced"] = same[0]
            return self._send(200, board)
        CORE_WRITES.append(("POST", path))
        return self._send(404, b"not found in the stub", "text/plain")

    def do_PUT(self):
        CORE_WRITES.append(("PUT", urlparse(self.path).path))
        self.rfile.read(int(self.headers.get("content-length") or 0))
        return self._send(404, b"not found in the stub", "text/plain")

    def do_DELETE(self):
        CORE_WRITES.append(("DELETE", urlparse(self.path).path))
        return self._send(404, b"not found in the stub", "text/plain")


LISTEN = (b"<script>window.__errs=[];addEventListener('error',function(e){__errs.push(String(e.message))});"
          b"addEventListener('unhandledrejection',function(e){__errs.push('unhandled: '+String(e.reason&&e.reason.message||e.reason))});</script>")


def upstream_json(path):
    with urllib.request.urlopen("http://127.0.0.1:%s%s" % (UI_PORT, path), timeout=20) as r:
        return json.loads(r.read().decode())


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

    def _live(self):
        """Answers a live route and returns True, or returns False for a path that is none."""
        path = urlparse(self.path).path
        length = int(self.headers.get("content-length") or 0)
        data = json.loads(self.rfile.read(length) or b"{}") if length else {}
        if path == "/api/bench/live" and self.command == "GET":
            self._send(200, {"live": STATE["live"]})
            return True
        if path == "/api/bench/live" and self.command == "DELETE":
            LIVE_DELETES.append(True)
            STATE["live"] = None
            self._send(204, b"", "text/plain")
            return True
        if path == "/api/bench/live/retract" and self.command == "POST":
            RETRACTS.append(data)
            offered = [x for x in STATE["leftovers"] if x["kind"] == data.get("kind") and x["id"] == data.get("id")]
            if not offered:
                self._send(409, b"not a leftover this bench's report offered", "text/plain; charset=utf-8")
            elif data.get("id") == "sniffer":
                self._send(200, {"ok": False, "text": "Core refused: the sniffer role is leased by session 4",
                                 "leftovers": STATE["leftovers"]})
            else:
                STATE["leftovers"] = [x for x in STATE["leftovers"] if x not in offered]
                self._send(200, {"ok": True, "text": "retracted", "leftovers": STATE["leftovers"]})
            return True
        m = re.match(r"^/api/bench/benches/([a-z0-9-]+)/live$", path)
        if m and self.command == "POST":
            slug = m.group(1)
            current = upstream_json("/api/bench/benches/" + slug).get("version")
            LIVE_POSTS.append({"slug": slug, "body": data, "file_version": current})
            if STATE["refuse_live"] or data.get("version") != current:
                self._send(409, ("embarch/topologies/%s.toml changed since it was read; reload it and save again" % slug).encode(),
                           "text/plain; charset=utf-8", {"x-embarch-refusal": "version-changed"})
            else:
                rep = report_for(slug, data.get("config"))
                STATE["live"] = rep["live"]
                STATE["leftovers"] = rep["leftovers"]
                self._send(200, rep)
            return True
        return False

    def _forward(self):
        path = urlparse(self.path).path
        live_route = path.startswith("/api/bench/live") or re.match(r"^/api/bench/benches/[^/]+/live$", path)
        if live_route and not REAL:
            if not self._live():
                self._send(404, b"no such live route in the drive's stub", "text/plain")
            return
        length = int(self.headers.get("content-length") or 0)
        data = self.rfile.read(length) if length else None
        if live_route and REAL:
            # The real routes answer; the drive only notes what was asked.
            body = json.loads(data or b"{}") if data else {}
            m = re.match(r"^/api/bench/benches/([a-z0-9-]+)/live$", path)
            if m and self.command == "POST":
                LIVE_POSTS.append({"slug": m.group(1), "body": body, "file_version": upstream_json("/api/bench/benches/" + m.group(1)).get("version")})
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


def element(css):
    return rq("POST", b + "/element", {"using": "css selector", "value": css})


def click(css, x=0, y=0):
    rq("POST", b + "/actions", {"actions": [{"type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"}, "actions": [
        {"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y},
        {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]})
    rq("DELETE", b + "/actions")


def settle():
    rq("POST", b + "/execute/async", {"script": "var done = arguments[arguments.length - 1];"
                                                " requestAnimationFrame(function(){ requestAnimationFrame(function(){ done(true); }); });", "args": []})


def key(value):
    rq("POST", b + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": [
        {"type": "keyDown", "value": value}, {"type": "keyUp", "value": value}]}]})
    rq("DELETE", b + "/actions")


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


def on_bench(slug):
    return wait_for("return document.getElementById('bench-path').textContent.indexOf(arguments[0] + '.toml') >= 0 &&"
                    " !!document.querySelector('#bench-svg [data-part]');", 8, [slug])


def pick_bench(slug):
    script("var s=document.getElementById('bench-pick'); s.value=arguments[0]; s.dispatchEvent(new Event('change'));", [slug])


def bar():
    return script("var l=document.getElementById('bench-live'), m=document.getElementById('bench-make-live'), s=document.getElementById('bench-stop-live');"
                  "return {state: l.hidden ? null : l.textContent, cls: l.className, make: m.hidden ? null : m.textContent, stop: !s.hidden};")


def panel_open(name):
    return script("var p=document.getElementById('topo-panel-'+arguments[0]); return !!p && !p.hidden && p.offsetParent !== null;", [name])


def report_text():
    return script("return document.getElementById('bench-live-body').innerText;")


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


def real_flow():
    st = wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent;", 6)
    check("REAL: the bar reads the binary's live record: no bench is live", st == "not live", bar())
    click("#bench-make-live")
    opened = wait_for("return !document.getElementById('topo-panel-live').hidden;", 15)
    check("REAL: Make live posts the config on screen and the version the browser read, and the report opens",
          opened and len(LIVE_POSTS) == 1 and LIVE_POSTS[0]["body"] == {"config": "A", "version": LIVE_POSTS[0]["file_version"]}, LIVE_POSTS)
    txt = report_text() or ""
    check("REAL: the report has its sections: applied, proposed, in Core and not on this bench",
          "Applied" in txt and "Proposed" in txt and "In Core, not on this bench" in txt, txt[:600])
    b1 = bar()
    check("REAL: the bar says this bench is live, wired as config A", b1["state"] == "live · A" and b1["stop"], b1)
    check("REAL: the live record agrees", upstream_json("/api/bench/live").get("live", {}).get("slug") == "live-bench")
    prop = script("var b=document.querySelector('#bench-live-body [data-live-confirm]'); return b ? b.getAttribute('data-live-confirm') : null;")
    check("REAL: the DUT's bound probe comes back as a proposal to confirm", prop is not None, txt[:800])
    if prop is not None:
        posts = len(LIVE_POSTS)
        click('#bench-live-body [data-live-confirm="%s"]' % prop)
        wait_for("var t=document.getElementById('bench-live-body').innerText; return t.indexOf('declared again') >= 0 || t.indexOf('not enrolled') >= 0;", 15)
        check("REAL: Confirm enrols the probe with the board type as the name", ENROLLS[-1:] and ENROLLS[-1].get("probe_serial") == DUT_PROBE and
              ENROLLS[-1].get("name") == "nrf54l15dk/nrf54l15/cpuapp" and ENROLLS[-1].get("role") == "dut", ENROLLS)
        check("REAL: and the live bench is declared again", len(LIVE_POSTS) == posts + 1, LIVE_POSTS[-1:])
    key("\ue00c")
    posts = len(LIVE_POSTS)
    flip("probe")
    click("#bench-save")
    wait_for("return document.getElementById('bench-dirty').hidden;", 8)
    wait_for("return arguments[0] < window.__liveEvents.length;", 10, [script("return window.__liveEvents.length;") - 1])
    time.sleep(1.0)
    check("REAL: saving the live bench declares it again with the new version",
          len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"]["version"] == file_version("live-bench"), LIVE_POSTS[-1:])
    key("\ue00c")
    click("#bench-stop-live")
    check("REAL: Stop being live clears the record", wait_for("return document.getElementById('bench-live').textContent === 'not live';", 8)
          and upstream_json("/api/bench/live").get("live") is None)
    check("REAL: no uncaught JS error or unhandled rejection", script("return window.__errs;") == [], script("return window.__errs;"))


shots = []
try:
    rq("POST", b + "/window/rect", {"width": 1700, "height": 1000})
    rq("POST", b + "/url", {"url": "about:blank"})
    rq("POST", b + "/url", {"url": PAGE + "/#topology"})
    on_bench("live-bench")
    script("window.__liveEvents=[]; document.addEventListener('embarch:live-changed', function(e){ __liveEvents.push(e.detail); });"
           "window.__roleEvents=[]; document.addEventListener('embarch:role-changed', function(e){ __roleEvents.push(e.detail); });")

    if REAL:
        real_flow()
    else:
        # --- the old diagram is gone -----------------------------------------------------
        gone = script("return {panel: !!document.getElementById('topo-panel-diagram'), button: !!document.querySelector('[data-topo-panel=\"diagram\"]'),"
                      " svg: !!document.getElementById('topology-diagram'), pool: !!document.getElementById('probes-pool'),"
                      " enrol: !!document.getElementById('assign-dialog'), picker: !!document.getElementById('role-board-dialog')};")
        check("the Old diagram panel, its button, its probe pool and its two dialogs are gone", not any(gone.values()), gone)

        # --- not live ---------------------------------------------------------------------
        st = wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent;", 6)
        check("the bar says no bench is live for this repo", st == "not live", bar())
        check("and offers Make live on the bench on screen", bar()["make"] == "Make live" and not bar()["stop"], bar())
        check("with no report yet, no Live report button", script("return document.getElementById('bench-live-btn').hidden;"))

        # --- make a saved bench live --------------------------------------------------------
        click("#bench-make-live")
        opened = wait_for("return !document.getElementById('topo-panel-live').hidden;", 6)
        check("Make live posts the bench as saved: the config on screen and the version it read", len(LIVE_POSTS) == 1 and
              LIVE_POSTS[0]["body"] == {"config": "A", "version": LIVE_POSTS[0]["file_version"]}, LIVE_POSTS)
        check("and the report opens as a panel", opened and panel_open("live"))
        txt = report_text() or ""
        check("an applied row says applied, a failed one says failed with Core's own text",
              "applied" in txt and "failed" in txt and "lease held by session 4" in txt, txt)
        check("the proposal is a row with Confirm, saying Confirm reads the probe's hardware ID",
              script("return !!document.querySelector('#bench-live-body [data-live-confirm=\"0\"]');") and "hardware ID" in txt, txt)
        check("a part not proposed says why", "not proposed" in txt and "no probe is bound on it" in txt)
        check("each leftover carries its own Retract, and nothing says it was retracted",
              script("return document.querySelectorAll('#bench-live-body [data-live-retract]').length;") == 2 and RETRACTS == [], txt)
        b1 = bar()
        check("the bar now says this bench is live, wired as config A, and offers Stop being live",
              b1["state"] == "live · A" and "bench-live-here" in b1["cls"] and b1["stop"] and b1["make"] is None, b1)
        check("the config tab that is the live wiring is marked", script(
            "var t=document.querySelector('#bench-configs [data-live-config]'); return t ? t.textContent : null;") == "A")
        check("embarch:live-changed says which bench and config", script("return window.__liveEvents;") == [{"slug": "live-bench", "config": "A"}],
              script("return window.__liveEvents;"))
        both_themes("live-report")
        check("making live touched no Core route from the page itself", CORE_WRITES == [], CORE_WRITES)

        # --- Confirm ------------------------------------------------------------------------
        posts = len(LIVE_POSTS)
        click('#bench-live-body [data-live-confirm="0"]')
        done = wait_for("var t=document.getElementById('bench-live-body').innerText; return t.indexOf('declared again') >= 0 ? t : '';", 8)
        check("Confirm runs the ordinary enrol with the proposal's fields verbatim — for a role, its board type as the name (decision 45)",
              ENROLLS == [{"role": "dut", "chip": "nRF54L15", "probe_serial": DUT_PROBE, "name": "nrf54l15dk/nrf54l15/cpuapp"}], ENROLLS)
        check("then declares the live bench again, with the version and config it is live with",
              len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"] == {"config": "A", "version": file_version("live-bench")}, LIVE_POSTS[-1:])
        check("and the fresh report says who was enrolled and what the enrol moved", bool(done) and "Enrolled dut" in done and OLD_PROBE in done, done)
        check("a confirmed enrolment tells the page the role's binding changed", script("return window.__roleEvents;") == [
            {"role": "dut", "probe_serial": DUT_PROBE}], script("return window.__roleEvents;"))

        # --- Retract ------------------------------------------------------------------------
        click('#bench-live-body [data-live-retract="0"]')
        wait_for("return document.querySelectorAll('#bench-live-body [data-live-retract]').length === 1;", 6)
        check("Retract sends only the leftover clicked", RETRACTS == [{"kind": "signal", "id": "old-sig"}], RETRACTS)
        check("and the list comes back from the answer, the signal gone", "old-sig" not in (report_text() or "") and "sniffer" in report_text())
        click('#bench-live-body [data-live-retract="0"]')
        refused = wait_for("var t=document.getElementById('bench-live-body').innerText; return t.indexOf('not retracted') >= 0 ? t : '';", 6)
        check("a Retract Core refuses says why on its row, and the leftover stays", "leased by session 4" in (refused or "") and
              script("return document.querySelectorAll('#bench-live-body [data-live-retract]').length;") == 1, refused)
        check("nothing else was retracted", RETRACTS == [{"kind": "signal", "id": "old-sig"}, {"kind": "role", "id": "sniffer"}], RETRACTS)

        # --- reopen without a new apply -------------------------------------------------------
        key("")
        check("Esc closes the report", not panel_open("live"))
        posts = len(LIVE_POSTS)
        click("#bench-live-btn")
        check("the Live report button reopens it without making live again", panel_open("live") and len(LIVE_POSTS) == posts)
        key("")

        # --- switching configs is a view change ---------------------------------------------------
        script("document.querySelector('#bench-configs [data-cfg=\"1\"]').click();")
        settle()
        b2 = bar()
        check("switching to config B on the live bench calls no route, and the A tab stays the live wiring",
              len(LIVE_POSTS) == posts and script("return document.querySelector('#bench-configs [data-live-config]').textContent;") == "A", b2)
        check("the bar offers to make B the live wiring instead, as its own click", b2["make"] == "Make B live", b2)
        script("document.querySelector('#bench-configs [data-cfg=\"0\"]').click();")
        settle()

        # --- write-through after a Save of the live bench ---------------------------------------
        flip("probe")
        click("#bench-save")
        wt = wait_for("return document.getElementById('bench-dirty').hidden && !document.getElementById('topo-panel-live').hidden;", 8)
        check("Saving the live bench declares it again, with the version the save answered, and shows the report",
              wt and len(LIVE_POSTS) == posts + 1 and LIVE_POSTS[-1]["body"] == {"config": "A", "version": file_version("live-bench")},
              LIVE_POSTS[-1:])
        check("the report says it was declared again after the save", "declared again" in (report_text() or ""), report_text())
        check("and embarch:live-changed says so", script("return window.__liveEvents.length;") == 3, script("return window.__liveEvents;"))
        key("")

        # --- a 409 on make live offers Reload ------------------------------------------------------
        pick_bench("other-bench")
        on_bench("other-bench")
        b3 = bar()
        check("another bench is not the live one, and says which is", b3["state"] == "live: live-bench" and b3["make"] == "Make live" and not b3["stop"], b3)
        STATE["refuse_live"] = True
        click("#bench-make-live")
        asked = wait_for("return document.getElementById('bench-dialog').style.display === 'block' ? document.getElementById('bench-dialog').innerText : '';", 6)
        check("a make-live refused because the file changed offers Reload, never a silent overwrite", "changed since it was read" in (asked or "") and
              "Reload" in asked, asked)
        script("document.getElementById('bench-dialog-cancel').click();")
        STATE["refuse_live"] = None

        # --- save and make live -----------------------------------------------------------------
        flip("probe")
        b4 = bar()
        check("with unsaved edits the button says Save and make live", b4["make"] == "Save and make live", b4)
        before = len(LIVE_POSTS)
        click("#bench-make-live")
        made = wait_for("return document.getElementById('bench-live').className.indexOf('bench-live-here') >= 0;", 8)
        flipped = tomllib.loads((TOPO / "other-bench.toml").read_text())
        check("it saves first, then makes live with the version that save answered", made and not dirty() and
              next(p for p in flipped["parts"] if p["id"] == "probe").get("flip") is True and
              len(LIVE_POSTS) == before + 1 and LIVE_POSTS[-1]["slug"] == "other-bench" and LIVE_POSTS[-1]["body"]["version"] == file_version("other-bench"),
              LIVE_POSTS[-1:])
        check("a bench with no config is made live with none", LIVE_POSTS[-1]["body"]["config"] is None and bar()["state"] == "live", bar())
        both_themes("live-bar")
        key("")

        # --- stop being live -------------------------------------------------------------------
        writes = list(CORE_WRITES)
        click("#bench-stop-live")
        stopped = wait_for("return document.getElementById('bench-live').textContent === 'not live';", 6)
        check("Stop being live clears the record and touches nothing in Core", stopped and LIVE_DELETES == [True] and CORE_WRITES == writes,
              [LIVE_DELETES, CORE_WRITES])
        check("embarch:live-changed says no bench is live", script("var e=window.__liveEvents; return e[e.length-1];") == {"slug": None, "config": None})

        # --- a record whose file is gone ---------------------------------------------------------
        STATE["live"] = {"slug": "gone-bench", "config": None, "since_utc_ms": NOW_MS, "missing": True}
        rq("POST", b + "/url", {"url": "about:blank"})
        rq("POST", b + "/url", {"url": PAGE + "/#topology"})
        on_bench("other-bench")
        miss = wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent.indexOf('gone') >= 0 ? l.textContent : '';", 6)
        check("a live record naming a file that is gone says so, never guesses", "gone-bench" in (miss or "") and "bench-live-missing" in bar()["cls"], bar())
        check("and offers to clear it", bar()["stop"] and bar()["make"] == "Make live", bar())
        click("#bench-stop-live")
        check("which clears it", wait_for("return document.getElementById('bench-live').textContent === 'not live';", 6))

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
