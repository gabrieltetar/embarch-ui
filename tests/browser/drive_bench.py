"""The Topology tab's bench canvas, in a real browser.

Phase 2 of embarch-bench-editor-proposal.md: a saved bench drawn read-only as
the hardware on the shelf, filling the Topology tab, with every other card a
panel over it. None of it is reachable from `cargo test` — every part, port
and cable is SVG that `bench.js` builds from the bench routes' JSON, the
reveal-on-hover is pointer state, and the glow is a class a snapshot toggles
without rebuilding anything.

**The bench routes are served from fixtures, through a proxy in front of the
real binary.** `tests/fixtures/bench/catalog.json` is the vocabulary and the
built-in catalog exactly as this build serves them, and `fixture.json` the
repo templates and benches (`src/bench/drive_fixture.rs` holds both to the
real types). The proxy answers `/api/bench/*` from them — `409` in the
contract's words while the UI has no project open — and passes everything
else, `/events` included, to the UI, so the glow reads the UI's own poll of
the stub Core below. It also puts an error listener at the top of the page,
so an error during load is caught too.

Ports: geckodriver at EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, a desk
port nothing listens on at +1, the fixture proxy the browser opens at +2, and
the stub Core at EMBARCH_STUB_PORT (default +3). Screenshots of the tab go to
EMBARCH_BENCH_SHOTS when it names a directory.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_bench.py
"""
import base64, http.client, json, os, subprocess, sys, tempfile, threading, time, urllib.request
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

REPO = Path(__file__).resolve().parents[2]
TOKEN = "stub-token"
UI_PORT = addr.ui_port(4893)
BASE = addr.gecko_url()
DESK_PORT = int(UI_PORT) + 1
PROXY_PORT = int(UI_PORT) + 2
CORE_PORT = int(os.environ.get("EMBARCH_STUB_PORT", str(int(UI_PORT) + 3)))
PAGE = "http://127.0.0.1:%d" % PROXY_PORT
SHOTS = os.environ.get("EMBARCH_BENCH_SHOTS")

FIX = REPO / "tests" / "fixtures" / "bench"
SERVED = json.loads((FIX / "catalog.json").read_text())
FX = json.loads((FIX / "fixture.json").read_text())
NO_PROJECT = "no project is open — pick a firmware repo in Project, at the bottom of the sidebar"


def routes():
    """The five GET answers, built from the two fixture files."""
    out = {
        "/api/bench/vocabulary": SERVED["vocabulary"],
        "/api/bench/parts": {"parts": SERVED["catalog"]["parts"] + FX["repo_parts"] + FX["newer_parts"],
                             "errors": SERVED["catalog"]["errors"] + FX["parts_errors"]},
        "/api/bench/benches": FX["benches"],
    }
    for k, v in FX.items():
        if k.startswith("bench/"):
            out["/api/bench/benches/" + k[len("bench/"):]] = v
    return out


PD = FX["bench/pd-bench-drp"]
PARTNER = "0671FF3833554B3043164817"
DEVKIT = "000683001234"
NOW_MS = 1758240000000

# Core's side. The dev kit's probe is attached from the start; the partner's
# comes and goes mid-run; the debug probe's serial is never here.
PROBES = [{"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": DEVKIT}]
SERIAL_PORTS = [{"port_name": "COM9", "detected_by": "enumerated", "vendor_id": None, "product_id": None,
                 "serial_number": "D607104", "product": None, "interface": 0}]
ENROLLED = [
    {"probe_serial": DEVKIT, "role": "dev-bench", "name": "nrf54l15dk/nrf54l15/cpuapp", "chip": "nRF54L15",
     "hardware_id": "aa:bb", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
    {"probe_serial": "DUTPROBE1", "role": "dut", "name": "nrf54l15dk/nrf54l15/cpuapp", "chip": "nRF54L15",
     "hardware_id": "cc:dd", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
    {"probe_serial": PARTNER, "role": "", "name": "pd-bench", "chip": "STM32G0B1RETx",
     "hardware_id": "ee:ff", "confirmed_at_utc_ms": NOW_MS, "link_serial": None},
]
ENROLLED.append({"probe_serial": "ZZZ999", "role": "sniffer", "name": "", "chip": "nRF52840",
                 "hardware_id": "11:22", "confirmed_at_utc_ms": NOW_MS, "link_serial": None})
CORE = {"up": True, "ports_fail": False}
MODE = {"bench": "fixture"}
SEEN = {"benches": 0}


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

    def _auth(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            self._send(401, b"unauthorized", "text/plain")
            return False
        if not CORE["up"]:
            self._send(503, b"down for the drive", "text/plain")
            return False
        return True

    def do_GET(self):
        if not self._auth():
            return
        path = urlparse(self.path).path
        answers = {"/status": {"status": "ok", "probes": PROBES, "study_designer_schema_version": 1},
                   "/probes/enrolled": ENROLLED, "/signals": [], "/serial-ports": SERIAL_PORTS, "/alerts": [],
                   "/studies": [], "/logs/recent": {"lines": []}}
        if path == "/serial-ports" and CORE["ports_fail"]:
            return self._send(500, b"serial enumeration failed in the stub", "text/plain")
        if path in answers:
            return self._send(200, answers[path])
        return self._send(404, b"not found in the stub", "text/plain")

    def do_POST(self):
        if not self._auth():
            return
        path = urlparse(self.path).path
        body = json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"{}")
        if path == "/validate":
            if body.get("role") == "dev-bench":
                return self._send(200, {"ok": True, "role": "dev-bench", "probe_serial": DEVKIT, "chip": "nRF54L15",
                                        "hardware_id": "aa:bb", "confirmed_at_utc_ms": NOW_MS,
                                        "validated_at_utc_ms": NOW_MS + 1000})
            return self._send(409, {"kind": "mismatch", "role": body.get("role"), "probe_serial": "DUTPROBE1",
                                    "chip": "nRF54L15", "recorded_hardware_id": "cc:dd", "live_hardware_id": "99:88",
                                    "reason": "hardware_id changed under role", "fix_it_url": "http://x/#topology"})
        return self._send(404, b"not found in the stub", "text/plain")

    def do_DELETE(self):
        if not self._auth():
            return
        path = urlparse(self.path).path
        global ENROLLED
        if path.startswith("/probes/enrolled/"):
            role = path.rsplit("/", 1)[1]
            if role == "sniffer":
                return self._send(500, b"the stub will not clear sniffer", "text/plain")
            ENROLLED = [b for b in ENROLLED if b["role"] != role]
            return self._send(204, b"", "text/plain")
        return self._send(404, b"not found in the stub", "text/plain")


# The page's first script: an error during load is an error too.
LISTEN = (b"<script>window.__errs=[];addEventListener('error',function(e){__errs.push(String(e.message))});"
          b"addEventListener('unhandledrejection',function(e){__errs.push('unhandled: '+String(e.reason&&e.reason.message||e.reason))});</script>")


def ui_project():
    with urllib.request.urlopen("http://127.0.0.1:%s/api/study-designer/project" % UI_PORT, timeout=10) as r:
        return json.loads(r.read().decode()).get("path")


class Proxy(BaseHTTPRequestHandler):
    # HTTP/1.0: a body ends when the connection does, which is what lets the
    # SSE stream pass through without re-chunking it.
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _forward(self):
        path = urlparse(self.path).path
        length = int(self.headers.get("content-length") or 0)
        data = self.rfile.read(length) if length else None
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
                pass  # the UI or the browser closed the stream
            return
        body = resp.read()
        if path == "/" and resp.status == 200:
            body = body.replace(b"<head>", b"<head>" + LISTEN, 1)
        if resp.status != 304:
            self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body and resp.status != 304:
            self.wfile.write(body)

    def do_GET(self):
        path = urlparse(self.path).path
        if path.startswith("/api/bench/"):
            if MODE["bench"] == "404":
                return self._send(404, b"Not Found", "text/plain")
            if not ui_project():
                return self._send(409, NO_PROJECT.encode(), "text/plain; charset=utf-8")
            if path == "/api/bench/benches":
                SEEN["benches"] += 1
            answers = routes()
            if path not in answers:
                return self._send(404, b"no such bench", "text/plain")
            return self._send(200, json.dumps(answers[path]).encode(), "application/json")
        return self._forward()

    do_POST = do_PUT = do_DELETE = _forward


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


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


core = serve(CORE_PORT, Core)
proxy = serve(PROXY_PORT, Proxy)

iso = tempfile.mkdtemp(prefix="embarch-ui-bench-drive-")
fw = iso + "/fw"
os.makedirs(fw + "/.git")
fw2 = iso + "/fw-two"
os.makedirs(fw2 + "/.git")
cfg = iso + "/ui.toml"
with open(cfg, "w") as f:
    f.write('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n' % (CORE_PORT, TOKEN))
env = dict(os.environ, EMBARCH_UI_CONFIG=cfg, EMBARCH_UI_PORT=UI_PORT, EMBARCH_DESK_PORT=str(DESK_PORT),
           EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data",
           XDG_CONFIG_HOME=iso + "/config", EMBARCH_UI_STATE=iso + "/recent-projects.json",
           HOME=iso + "/home")
ui = subprocess.Popen([os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
    "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


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


def element(css):
    return rq("POST", b + "/element", {"using": "css selector", "value": css})


def pointer(actions):
    rq("POST", b + "/actions", {"actions": [{"type": "pointer", "id": "mouse",
                                             "parameters": {"pointerType": "mouse"}, "actions": actions}]})
    rq("DELETE", b + "/actions")


def move_to(css, x=0, y=0):
    pointer([{"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y}])


def click(css, x=0, y=0):
    pointer([{"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y},
             {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}])


def key(value):
    rq("POST", b + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": [
        {"type": "keyDown", "value": value}, {"type": "keyUp", "value": value}]}]})
    rq("DELETE", b + "/actions")


def open_page():
    rq("POST", b + "/url", {"url": "about:blank"})
    rq("POST", b + "/url", {"url": PAGE + "/#topology"})


def shot(name):
    if not SHOTS:
        return None
    os.makedirs(SHOTS, exist_ok=True)
    time.sleep(0.4)
    path = os.path.join(SHOTS, name)
    with open(path, "wb") as f:
        f.write(base64.b64decode(rq("GET", b + "/screenshot")))
    return path


def pick_bench(slug):
    script("var s=document.getElementById('bench-pick'); s.value=arguments[0]; s.dispatchEvent(new Event('change'));", [slug])
    wait_for("return document.getElementById('bench-path').textContent.indexOf(arguments[0]) >= 0 &&"
             " !!document.querySelector('#bench-svg [data-part]');", 8, [slug])


def pick_config(i):
    script("document.querySelector('#bench-configs [data-cfg=\"%d\"]').click();" % i)


def panel_open(name):
    return script("var p=document.getElementById('topo-panel-'+arguments[0]); return !!p && !p.hidden && p.offsetParent !== null;", [name])


PORTS = ("return Array.from(document.querySelectorAll('#bench-svg [data-part=\"' + arguments[0] + '\"] [data-conn]'))"
         ".map(function(g){return [g.getAttribute('data-conn'), g.getAttribute('data-x'), g.getAttribute('data-y'),"
         " g.classList.contains('bn-free')];});")
CABLES = ("return Array.from(document.querySelectorAll('#bench-svg .bn-cable')).map(function(p){"
          "return [p.getAttribute('data-cable'), p.classList.contains('bn-cfg'), p.getAttribute('d')];});")
NOTES = "return Array.from(document.querySelectorAll('#bench-notes li')).map(function(l){return l.textContent;});"
INSPECTOR = "var i=document.getElementById('bench-inspector'); return i.hidden ? null : i.innerText.replace(/\\s+/g, ' ');"

shots = []
try:
    rq("POST", b + "/window/rect", {"width": 1500, "height": 950})
    open_page()

    # --- no project open, then the first one opened -----------------------
    state = wait_for("var s=document.getElementById('bench-state'); return !s.hidden && s.textContent.indexOf('no project') >= 0 ? s.textContent : '';")
    check("with no project open the canvas says so in the server's words, one quiet line",
          state == NO_PROJECT and script("return document.getElementById('bench-state').className.indexOf('placeholder-note') >= 0 &&"
                                         " document.getElementById('bench-body').hidden && !document.querySelector('#bench-svg [data-part]');"), state)
    script("document.getElementById('project-button').click();")
    time.sleep(0.4)
    script("document.getElementById('project-path').value=arguments[0]; document.getElementById('project-open').click();", [fw])
    drawn = wait_for("return document.querySelectorAll('#bench-svg [data-part]').length;", 10)
    check("the first project opened is the one the canvas loads", drawn and script(
        "return document.getElementById('bench-path').textContent;") == "embarch/topologies/pd-bench-drp.toml", drawn)
    script("var d=document.getElementById('project-dialog'); if (d && d.style.display==='block') document.getElementById('project-cancel').click();")

    # --- the tab fills the screen ------------------------------------------
    lay = script("var c=document.querySelector('.content'), w=document.getElementById('bench-wrap').getBoundingClientRect(),"
                 " bar=document.getElementById('topo-bar').getBoundingClientRect();"
                 "return {pageScroll: document.scrollingElement.scrollHeight - innerHeight, contentScroll: c.scrollHeight - c.clientHeight,"
                 " wrapH: w.height, wrapW: w.width, vh: innerHeight, barBottom: bar.bottom, wrapTop: w.top};")
    check("the Topology tab fills the screen with no page scroll",
          lay["pageScroll"] <= 0 and lay["contentScroll"] <= 0, lay)
    check("and the canvas takes the space under the bar", lay["wrapH"] > lay["vh"] * 0.6 and lay["wrapTop"] >= lay["barBottom"] - 1, lay)
    bar = script("var b=document.getElementById('topo-bar'); return [!!b.querySelector('#bench-pick'), !!b.querySelector('#topo-validate'),"
                 " Array.from(b.querySelectorAll('.topo-menu [data-topo-panel]')).map(function(x){return x.textContent;})];")
    check("the bar holds the bench picker, Validate topology and one button per panel",
          bar == [True, True, ["Signals", "Board types", "Saved benches", "Old diagram"]], bar)
    check("every panel starts closed", not any(panel_open(n) for n in ["signals", "boards", "saved", "diagram", "validate"]))
    click('[data-topo-panel="signals"]')
    geo = script("var p=document.getElementById('topo-panel-signals').getBoundingClientRect(),"
                 " bar=document.getElementById('topo-bar').getBoundingClientRect(), cs=getComputedStyle(document.getElementById('topo-panel-signals'));"
                 "return {top: p.top, barBottom: bar.bottom, bottom: p.bottom, vh: innerHeight, overflow: cs.overflowY};")
    check("a panel button opens its panel over the canvas, anchored under the bar",
          panel_open("signals") and geo["top"] >= geo["barBottom"] and geo["bottom"] <= geo["vh"] and geo["overflow"] == "auto", geo)
    check("holding that card", script("return !!document.querySelector('#topo-panel-signals #signals-table-body');"))
    click('[data-topo-panel="signals"]')
    check("its own button closes it", not panel_open("signals"))
    click('[data-topo-panel="boards"]')
    check("opening a panel moves focus into it",
          script("return document.getElementById('topo-panel-boards').contains(document.activeElement);"))
    key("")
    check("Esc closes it, and focus goes back to its button", not panel_open("boards") and
          script("return document.activeElement === document.querySelector('[data-topo-panel=\"boards\"]');"))
    click('[data-topo-panel="saved"]')
    click('[data-topo-panel="diagram"]')
    check("one panel at a time", panel_open("diagram") and not panel_open("saved"))
    check("the old diagram, its probe pool and its drop targets live in their panel",
          script("var p=document.getElementById('topo-panel-diagram');"
                 "return !!p.querySelector('#topology-diagram [data-enroll-role=\"dut\"]') && !!p.querySelector('#probes-pool');"))
    click("#bench-wrap", 0, 200)
    check("a click outside closes it", not panel_open("diagram"))
    click('[data-topo-panel="signals"]')
    script("document.getElementById('sig-declare').click();")
    wait_for("return document.getElementById('sig-dialog').style.display === 'block';", 4)
    script("document.getElementById('sig-name').value='half-typed';"
           "document.getElementById('prompt-backdrop').style.display='block'; document.getElementById('prompt-dialog').style.display='block';")
    key("")
    check("M1: Esc with Core's prompt over the signal dialog closes neither, and keeps what was typed",
          script("return document.getElementById('sig-dialog').style.display === 'block' &&"
                 " document.getElementById('prompt-dialog').style.display === 'block' &&"
                 " document.getElementById('sig-name').value === 'half-typed';") and panel_open("signals"))
    script("document.getElementById('prompt-dialog').style.display='none'; document.getElementById('prompt-backdrop').style.display='none';")
    key("")
    check("with the prompt gone, Esc closes the signal dialog and leaves its panel",
          script("return document.getElementById('sig-dialog').style.display !== 'block';") and panel_open("signals"))
    check("L4: and focus goes back to the panel's first control",
          script("return document.activeElement === document.getElementById('sig-declare');"))
    key("")

    # --- every look --------------------------------------------------------
    looks = dict(script("return Array.from(document.querySelectorAll('#bench-svg [data-part]')).map(function(g){"
                        "return [g.getAttribute('data-part'), g.getAttribute('data-look')];});"))
    check("every part on the bench is drawn", sorted(looks) == sorted(p["id"] for p in PD["parts"]), sorted(looks))
    check("every served look is drawn by its own look", set(SERVED["vocabulary"]["looks"]) <= set(looks.values()),
          sorted(set(looks.values())))
    check("a look this build does not draw is drawn as a board, not dropped", looks.get("odd") == "board")
    sub = lambda pid: script("return document.querySelector('#bench-svg [data-part=\"'+arguments[0]+'\"] .bn-sub').textContent;", [pid])
    check("a part whose template is gone says the template is missing", "template missing" in sub("ghost"), sub("ghost"))
    check("a part whose template would not load says it is unreadable, not missing",
          "template unreadable" in sub("broken"), sub("broken"))
    check("its cable still lands, on a connector named by the cable",
          script("return !!document.querySelector('#bench-svg [data-conn=\"ghost.j1\"].bn-port-missing');"))
    check("a stacking connector is never drawn as a port: the shield sits on its board instead",
          not script("return !!document.querySelector('#bench-svg [data-conn=\"partner.stack\"], #bench-svg [data-conn=\"drp.stack\"]');"))
    g2 = script("function r(id){var g=document.querySelector('#bench-svg [data-part=\"'+id+'\"] .bn-body');"
                "return [+g.getAttribute('x'), +g.getAttribute('y'), +g.getAttribute('height')];} return [r('drp'), r('partner')];")
    check("the mounted shield is drawn on top of its board", g2[0][0] == g2[1][0] and abs(g2[0][1] + g2[0][2] - g2[1][1]) < 0.5, g2)
    check("the analyzer is a plain case: no vendor colours, two USB-C at the sides and USB on top",
          script("var p=document.querySelector('#bench-svg [data-part=\"cy4500\"]');"
                 "return p.querySelector('.bn-body').classList.contains('bn-case') && !p.querySelector('.bn-pcb, .bn-pcb-2');")
          and sorted(c[0] for c in script(PORTS, ["cy4500"])) == ["cy4500.in", "cy4500.out", "cy4500.usb"])
    move_to('#bench-svg [data-part="breakout"] .bn-body')
    flip = script("var p=document.querySelector('#bench-svg [data-part=\"breakout\"] .bn-body');"
                  "function x(k){var g=document.querySelector('#bench-svg [data-conn=\"'+k+'\"]');return g?+g.getAttribute('data-x'):null;}"
                  "return [x('breakout.usb-c'), x('breakout.banana'), +p.getAttribute('x'), +p.getAttribute('width')];")
    check("a flipped part draws each side's connectors on the other", flip[0] == flip[2] + flip[3] and flip[1] == flip[2], flip)
    move_to('#bench-svg [data-part="odd"] .bn-body')
    lvds = script("var g=document.querySelector('#bench-svg [data-conn=\"odd.lvds\"]');"
                  "return g ? [g.getAttribute('data-kind'), Array.from(g.querySelectorAll('.bn-dot')).map(function(d){return d.tagName;})] : null;")
    check("a kind this build does not draw is drawn as a plain dot, not dropped", lvds == ["lvds", ["circle"]], lvds)

    # --- reveal on intent, in config B ---------------------------------------
    move_to("#bench-pick")
    pick_config(1)
    before = script(PORTS, ["dut"])
    cables_before = script(CABLES)
    check("in config B the DUT shows only its wired connectors",
          [c[0] for c in before] == ["dut.swd", "dut.usb-c-2"] and not any(c[3] for c in before), before)
    move_to('#bench-svg [data-part="dut"] .bn-body')
    after = script(PORTS, ["dut"])
    check("hovering it reveals the free ones, USB-C 1 among them though the template lists it before a wired port",
          [c[0] for c in after] == ["dut.swd", "dut.usb-c-2", "dut.usb-c-1", "dut.dc-in"]
          and [c[0] for c in after if c[3]] == ["dut.usb-c-1", "dut.dc-in"], after)
    check("and no wired connector moved", all(any(a[:3] == w[:3] for a in after) for w in before), (before, after))
    check("nor any cable", script(CABLES) == cables_before)
    move_to('#bench-svg [data-part="drp"] .bn-body')
    drp = script(PORTS, ["drp"])
    check("a mounted shield reveals its free row without moving its wired ones",
          sorted(c[0] for c in drp) == ["drp.cn2", "drp.cn3", "drp.usb-c"] and script(CABLES) == cables_before, drp)
    move_to("#bench-pick")
    check("leaving the part hides them again", [c[0] for c in script(PORTS, ["dut"])] == ["dut.swd", "dut.usb-c-2"])
    pick_config(0)

    # --- the hub -----------------------------------------------------------
    hub = script("return Array.from(document.querySelectorAll('#bench-svg [data-part=\"host\"] .bn-clabel')).map(function(t){return t.textContent;});")
    check("this machine has one port per cable, named by what it carries, top to bottom",
          hub == ["Debug probe → DUT", "CY4500", "pd-bench", "eload", "nRF54L15 DK",
                  "aux board · USB", "aux board · UART", "Odd widget", "USB-UART bridge"], hub)

    # --- bench-level solid, config dashed; config tabs ---------------------
    cfgs = PD["configs"]
    cab = script(CABLES)
    check("bench-level cables are solid, one per cable to this machine", len([c for c in cab if not c[1]]) == len(PD["cables"]), cab)
    check("config cables are dashed, one per cable in the config on screen", len([c for c in cab if c[1]]) == len(cfgs[0]["cables"]))
    solid_a = sorted(c[2] for c in cab if not c[1])
    pick_config(1)
    cab_b = script(CABLES)
    check("a tab switches which config's dashed cables are drawn",
          len([c for c in cab_b if c[1]]) == len(cfgs[1]["cables"]) - 1
          and script("return !!document.querySelector('#bench-svg [data-conn=\"psu.out\"]') && !document.querySelector('#bench-svg [data-conn=\"drp.cn2\"]');"))
    check("and leaves the bench-level ones where they were", sorted(c[2] for c in cab_b if not c[1]) == solid_a)
    check("a config cable to a part the bench lacks is named, not drawn", any("nowhere" in n for n in script(NOTES)))
    pick_config(0)
    check("switching back restores the first config's cables", script(CABLES) == cab)

    # --- a selection follows its cable, not its index ----------------------
    click('#bench-svg [data-conn="dut.swd"]')
    sel_swd = script("var p=document.querySelector('#bench-svg .bn-cable.bn-sel'); return p && p.getAttribute('data-cable');")
    pick_config(1)
    check("a cable selected in one config stays selected in another that has it",
          script("var p=document.querySelector('#bench-svg .bn-cable.bn-sel'); return p && p.getAttribute('data-cable');") == sel_swd and sel_swd)
    pick_config(0)
    click('#bench-svg [data-conn="dut.usb-c-1"]')
    pick_config(1)
    check("and one the other config lacks is dropped, never swapped for the cable at its index",
          not script("return !!document.querySelector('#bench-svg .bn-cable.bn-sel');") and script(INSPECTOR) is None)
    pick_config(0)

    # --- select, inspect ---------------------------------------------------
    check("nothing selected, no inspector", script(INSPECTOR) is None)
    click('#bench-svg [data-part="partner"] .bn-body')
    ins = script(INSPECTOR) or ""
    pos = script("var i=document.getElementById('bench-inspector').getBoundingClientRect(), w=document.getElementById('bench-wrap').getBoundingClientRect();"
                 "return [Math.round(w.right - i.right), Math.round(i.top - w.top)];")
    # Phase 3 made the inspector the editor's (drive_bench_edit.py drives it):
    # its controls are the part's label, role, flip and delete.
    check("a selected part opens an inspector floating on the canvas's right, holding the part's own controls",
          0 <= pos[0] <= 20 and 0 <= pos[1] <= 20 and script(
              "var i=document.getElementById('bench-inspector');"
              "return !!i.querySelector('[data-ed=\"label\"]') && !!i.querySelector('[data-ed=\"role\"]') &&"
              " !!i.querySelector('[data-act=\"flip\"]') && !!i.querySelector('[data-act=\"delete\"]');"), pos)
    check("it says the template, role, connectors and what each cable reaches",
          all(t in ins for t in ["pd-bench", "NUCLEO-G0B1RE", "built in", "Role", "ST-LINK USB", "This machine", "X-NUCLEO-DRP1M1"]), ins)
    check("and each binding as text: a serial, an interface on that device",
          "probe " + PARTNER in ins and "vcom interface 1" in ins, ins)
    click('#bench-svg [data-part="aux"] .bn-body')
    ins = script(INSPECTOR) or ""
    check("a binding in table form reads as its serial and interface, never [object Object]",
          "AUX-UART · interface 0" in ins and "object Object" not in ins, ins)
    click('#bench-svg [data-part="broken"] .bn-body')
    ins = script(INSPECTOR) or ""
    check("an unreadable template's part carries the file's error", "unreadable" in ins and "usbc" in ins, ins)
    click('#bench-svg [data-conn="dut.usb-c-1"]')
    ins = script(INSPECTOR) or ""
    check("a wired connector selects its cable, which says its ends and its config",
          "DUT · USB-C 1" in ins and "CY4500 · USB-C" in ins and cfgs[0]["name"] in ins, ins)

    # --- the live glow, and a snapshot that rebuilds nothing ---------------
    LIVE = "return Array.from(document.querySelectorAll('#bench-svg .bn-live')).map(function(e){return e.getAttribute('data-live');});"
    wait_for("return document.querySelectorAll('#bench-svg .bn-live').length > 0;", 12)
    first = script(LIVE)
    check("a usb-host connector whose bound probe is enumerated glows, with its cable and its port on this machine",
          set(first) == {"devkit.usb"} and len(first) == 5, first)   # port and ring, cable, hub port and ring
    check("a bound probe Core does not enumerate does not", "probe.usb" not in first)
    click('#bench-svg [data-part="partner"] .bn-body')
    move_to("#bench-pick")
    script("window.__benchNode=document.querySelector('#bench-svg [data-part=\"partner\"]');"
           "window.__benchSvg=document.querySelector('#bench-svg').firstElementChild;")
    PROBES.append({"identifier": "STLink", "vendor_id": 1155, "product_id": 14158, "serial_number": PARTNER})
    lit = wait_for("return !!document.querySelector('#bench-svg [data-live=\"partner.stlink\"].bn-live');", 14)
    check("a snapshot that enumerates the partner's probe lights its connector", lit)
    check("without rebuilding the canvas",
          script("return document.querySelector('#bench-svg [data-part=\"partner\"]') === window.__benchNode &&"
                 " document.querySelector('#bench-svg').firstElementChild === window.__benchSvg;"))
    check("so the selection survives it", script("return window.__benchNode.classList.contains('bn-sel') &&"
                                                  " (document.getElementById('bench-inspector').innerText||'').indexOf('pd-bench') === 0;"))
    check("and the inspector marks the binding that is enumerated",
          script("var r=document.querySelector('#bench-inspector [data-live-binding=\"partner|stlink|probe\"]'); return !!r && r.classList.contains('bn-live');"))
    SERIAL_PORTS.append({"port_name": "COM12", "detected_by": "enumerated", "vendor_id": None, "product_id": None,
                         "serial_number": "LOAD-0001", "product": None, "interface": None})
    check("a serial-port binding glows when Core enumerates that port's serial",
          wait_for("return !!document.querySelector('#bench-svg [data-live=\"eload.usb\"].bn-live');", 14))
    if SHOTS:
        shots.append(shot("bench-tab-dark.png"))
        script("document.documentElement.setAttribute('data-theme','light');")
        shots.append(shot("bench-tab-light.png"))
        script("document.documentElement.setAttribute('data-theme','dark');")
    BRIDGE = "return !!document.querySelector('#bench-svg [data-live=\"bridge.usb\"].bn-live');"
    ch340 = {"port_name": "COM20", "detected_by": "enumerated", "vendor_id": 6790, "product_id": 29987,
             "serial_number": None, "product": None, "interface": None}
    check("a bridge with no serial is not lit before its port appears", not script(BRIDGE))
    SERIAL_PORTS.append(dict(ch340))
    check("a vid/pid binding glows when exactly one port has that vid and pid", wait_for(BRIDGE, 14))
    SERIAL_PORTS.append(dict(ch340, port_name="COM21"))
    check("and goes out when two do: which one is ambiguous", wait_for("return !(function(){" + BRIDGE + "})();", 14))
    SERIAL_PORTS.pop()
    SERIAL_PORTS.pop()
    CORE["ports_fail"] = True
    portline = wait_for("var s=document.getElementById('bench-live-state'); return !s.hidden ? s.textContent : '';", 14)
    check("a serial-port list Core could not read leaves port glows unknown, and says so",
          "serial ports" in (portline or "") and not script("return !!document.querySelector('#bench-svg [data-live=\"eload.usb\"].bn-live');"),
          portline)
    check("while probe glows, from a list Core did read, stay lit",
          script("return !!document.querySelector('#bench-svg [data-live=\"devkit.usb\"].bn-live');"))
    CORE["ports_fail"] = False
    check("and the eload lights again once the list is back",
          wait_for("return document.getElementById('bench-live-state').hidden &&"
                   " !!document.querySelector('#bench-svg [data-live=\"eload.usb\"].bn-live');", 14))
    SERIAL_PORTS.pop()
    PROBES.pop()
    dark = wait_for("return !document.querySelector('#bench-svg [data-live=\"partner.stlink\"].bn-live') &&"
                    " !document.querySelector('#bench-svg [data-live=\"eload.usb\"].bn-live');", 14)
    check("a snapshot without them puts both out, still rebuilding nothing",
          dark and script("return document.querySelector('#bench-svg [data-part=\"partner\"]') === window.__benchNode &&"
                          " window.__benchNode.classList.contains('bn-sel');"))
    CORE["up"] = False
    unknown = wait_for("var s=document.getElementById('bench-live-state'); return !s.hidden ? s.textContent : '';", 14)
    check("with Core not answering, the glow goes out and the canvas says enumeration is unknown",
          "unknown" in (unknown or "") and not script("return document.querySelectorAll('#bench-svg .bn-live').length;"), unknown)
    CORE["up"] = True
    check("and stops saying so when Core answers again",
          wait_for("return document.getElementById('bench-live-state').hidden && document.querySelectorAll('#bench-svg .bn-live').length > 0;", 14))

    # --- Validate's verdicts, on the parts ---------------------------------
    VERDICT = "var v=document.querySelector('#bench-svg [data-verdict=\"'+arguments[0]+'\"]'); return v ? [v.getAttribute('class'), v.textContent] : null;"
    check("no verdict is drawn before Validate has run", not script("return document.querySelectorAll('#bench-svg [data-verdict]').length;"))
    script("document.getElementById('topo-validate').click();")
    wait_for("return document.getElementById('topo-validate-report').textContent.indexOf('Role') >= 0;", 10)
    check("Validate opens its report as a panel", panel_open("validate"))
    vd, vk, vp = script(VERDICT, ["dut"]), script(VERDICT, ["devkit"]), script(VERDICT, ["partner"])
    check("the verdict lands under the part holding the role: a refused DUT fails",
          vd and "bn-v-fail" in vd[0] and "failed" in vd[1], vd)
    check("a dev bench Core re-read passes", vk and "bn-v-pass" in vk[0] and "validated" in vk[1], vk)
    check("and a bench board's lands on the part bound to its probe", vp and "bn-v-pass" in vp[0], vp)
    script("window.confirm=function(){return true}; document.querySelector('#topo-validate-report [data-unenroll-role=\"sniffer\"]').click();")
    err = wait_for("var e=document.getElementById('topo-validate-error'); return e.offsetParent !== null ? e.textContent : '';", 6)
    check("a leftover role's clear that fails says so on the Validate panel, where its control is",
          "500" in (err or "") and panel_open("validate"), err)
    key("")
    check("the bar keeps a Report button once a pass has run", script("return !document.getElementById('topo-report').hidden;"))
    click("#topo-report")
    check("which reopens the last report without a new pass", panel_open("validate") and
          "sniffer" in script("return document.getElementById('topo-validate-report').textContent;"))
    script("document.getElementById('topo-validate').click();")
    check("L3: a new Validate pass starts with the last pass's error cleared",
          script("var e=document.getElementById('topo-validate-error'); return e.style.display === 'none' && e.textContent === '';"))
    wait_for("return !document.getElementById('topo-validate').disabled &&"
             " document.getElementById('topo-validate-report').textContent.indexOf('Role') >= 0;", 10)
    key("")
    click('[data-topo-panel="diagram"]')
    script("document.querySelector('[data-board-role=\"dut\"] text').dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    wait_for("return document.getElementById('role-board-dialog').style.display === 'block';", 5)
    key("")
    check("Esc over the board-type dialog closes the dialog, not the Old diagram panel under it",
          script("return document.getElementById('role-board-dialog').style.display !== 'block';") and panel_open("diagram"))
    script("document.querySelector('[data-board-role=\"dut\"] text').dispatchEvent(new MouseEvent('click',{bubbles:true}));")
    wait_for("return document.getElementById('role-board-dialog').style.display === 'block';", 5)
    script("window.confirm=function(){return true}; document.getElementById('role-board-retract').click();")
    gone = wait_for("return !document.querySelector('#bench-svg [data-verdict=\"dut\"]');", 6)
    check("a role's binding changing drops its verdict from its part, and only its", gone and script(VERDICT, ["devkit"]), script(VERDICT, ["devkit"]))
    key("")

    # --- the Saved benches panel and the canvas read the same files ---------
    pick_config(1)
    click('#bench-svg [data-part="partner"] .bn-body')
    script("document.getElementById('bench-zoom-in').click();")
    held = script("return [document.getElementById('bench-svg').getAttribute('viewBox'),"
                  " document.querySelector('#bench-configs [aria-selected=\"true\"]').getAttribute('data-cfg')];")
    click('[data-topo-panel="saved"]')
    script("document.getElementById('topo-profile-save').click();")
    wait_for("return document.getElementById('topo-save-dialog').style.display === 'block';", 4)
    if SHOTS:
        shots.append(shot("bench-tab-dialog-over-panel.png"))
    key("")
    check("Esc over Save as\u2026 closes the dialog, not the Saved benches panel under it",
          script("return document.getElementById('topo-save-dialog').style.display !== 'block';") and panel_open("saved"))
    check("and focus lands on the panel's first control, not a field the dialog hid",
          script("return document.activeElement === document.getElementById('topo-profile-picker');"))
    FX["benches"]["benches"].append({"slug": "bench-a", "name": "bench a", "legacy": False})
    FX["bench/bench-a"] = dict(FX["bench/ble-dev-bench"], slug="bench-a", name="bench a", legacy=False)
    seen = SEEN["benches"]
    script("document.getElementById('topo-profile-save').click();")
    time.sleep(0.3)
    script("document.getElementById('topo-save-name').value='bench a'; document.getElementById('topo-save-confirm').click();")
    listed = wait_for("return Array.from(document.getElementById('bench-pick').options).some(function(o){return o.value==='bench-a';});", 8)
    check("a bench saved from the Saved benches panel appears in the canvas's picker", listed and SEEN["benches"] > seen, SEEN)
    check("and the bench on screen stays the one that was", script("return document.getElementById('bench-pick').value;") == "pd-bench-drp")
    check("with its zoom, its config and its selection",
          script("return [document.getElementById('bench-svg').getAttribute('viewBox'),"
                 " document.querySelector('#bench-configs [aria-selected=\"true\"]').getAttribute('data-cfg')];") == held
          and script("return !!document.querySelector('#bench-svg [data-part=\"partner\"].bn-sel');"), held)
    FX["benches"]["benches"] = [r for r in FX["benches"]["benches"] if r["slug"] != "bench-a"]
    script("window.confirm=function(){return true}; var s=document.getElementById('topo-profile-picker'); s.value='bench-a';"
           "document.getElementById('topo-profile-delete').click();")
    unlisted = wait_for("return !Array.from(document.getElementById('bench-pick').options).some(function(o){return o.value==='bench-a';});", 8)
    check("and one deleted there leaves it", unlisted)
    key("")
    pick_config(0)

    # --- the desk's bench, reviewed from the banner -------------------------
    script("document.dispatchEvent(new CustomEvent('embarch:desk',{detail:{desk:{id:'drive-a',name:'drive a',firmware_repos:[arguments[0]]}}}));"
           "document.dispatchEvent(new CustomEvent('embarch:desk',{detail:{desk:{id:'drive-b',name:'drive b',bench:'no-such-bench',firmware_repos:[arguments[0]]}}}));", [fw])
    wait_for("var b=document.getElementById('desk-bench-review'); return !!b;", 6)
    script("document.getElementById('desk-bench-review').click();")
    said = wait_for("var e=document.getElementById('topo-profile-error'); return e.offsetParent !== null ? e.textContent : '';", 8)
    check("the desk banner's review opens the Saved benches panel, where its errors and report land",
          panel_open("saved") and "no-such-bench" in (said or ""), said)
    key("")
    check("L5: a panel opened from code returns focus to its own bar button when it closes",
          not panel_open("saved") and script("return document.activeElement === document.querySelector('.topo-bar [data-topo-panel=\"saved\"]');"))

    # --- pan, zoom, fit ----------------------------------------------------
    vb = lambda: [float(v) for v in script("return document.getElementById('bench-svg').getAttribute('viewBox');").split()]
    script("document.querySelector('#bench-svg').dispatchEvent(new PointerEvent('pointerdown',{bubbles:true,button:0,clientX:5,clientY:5}));"
           "document.querySelector('#bench-svg').dispatchEvent(new PointerEvent('pointerup',{bubbles:true,button:0,clientX:5,clientY:5}));")
    script("document.getElementById('bench-fit').click();")
    v0 = vb()
    rq("POST", b + "/actions", {"actions": [{"type": "wheel", "id": "wheel", "actions": [
        {"type": "scroll", "x": 0, "y": 0, "deltaX": 0, "deltaY": -240, "origin": element("#bench-svg")}]}]})
    rq("DELETE", b + "/actions")
    v1 = vb()
    check("the wheel zooms in, and nothing scrolls",
          v1[2] < v0[2] and script("return document.querySelector('.content').scrollTop + document.scrollingElement.scrollTop;") == 0, (v0, v1))
    empty = script("var w=document.getElementById('bench-wrap').getBoundingClientRect();"
                   "function bare(x,y){var e=document.elementFromPoint(x,y);return !!e&&(e.id==='bench-grid'||e.id==='bench-svg');}"
                   "for(var y=w.top+60;y<w.bottom-120;y+=15)for(var x=w.left+60;x<w.right-360;x+=15)"
                   "if(bare(x,y)&&bare(x+120,y+40))return [Math.round(x-(w.left+w.width/2)),Math.round(y-(w.top+w.height/2))];"
                   "return null;")
    check("the canvas has bare grid to pan from", empty is not None)
    click('#bench-svg [data-part="partner"] .bn-body')
    pointer([{"type": "pointerMove", "duration": 0, "origin": element("#bench-wrap"), "x": empty[0], "y": empty[1]},
             {"type": "pointerDown", "button": 0},
             {"type": "pointerMove", "duration": 100, "origin": "pointer", "x": 120, "y": 40},
             {"type": "pointerUp", "button": 0}])
    v2 = vb()
    check("dragging empty space pans and keeps the selection",
          v2[0] < v1[0] and v2[1] < v1[1] and abs(v2[2] - v1[2]) < 0.01
          and script("return !!document.querySelector('#bench-svg [data-part=\"partner\"].bn-sel');"), (v1, v2))
    script("document.getElementById('bench-fit').click();")
    check("Fit brings the whole bench back", vb() == v0, (v0, vb()))

    # --- the legacy bench: a binding in table form, matched by interface ---
    pick_bench("ble-dev-bench")
    check("a legacy file says how it was laid out", "no [[parts]]" in script("return document.getElementById('bench-path').textContent;"))
    check("a bench with no configs shows no config tabs", script("return document.getElementById('bench-config-row').hidden;"))
    click('#bench-svg [data-part="dev-bench"] .bn-body')
    ins = script(INSPECTOR) or ""
    check("its dev-bench link reads as a serial and an interface", "D607104 · interface 2" in ins and "object Object" not in ins, ins)
    VCOM = "var r=document.querySelector('#bench-inspector [data-live-binding=\"dev-bench|usb|vcom\"]'); return !!r && r.classList.contains('bn-live');"
    check("the same serial on another interface is not that port", not script(VCOM))
    SERIAL_PORTS.append({"port_name": "COM17", "detected_by": "enumerated", "vendor_id": None, "product_id": None,
                         "serial_number": "D607104", "product": None, "interface": 2})
    check("the port with that serial and interface is", wait_for(VCOM, 14))

    # --- ids that are object keys, two parts under one id, a huge bench -----
    pick_bench("edge-ids")
    parts = script("return Array.from(document.querySelectorAll('#bench-svg [data-part]')).map(function(g){return g.getAttribute('data-part');});")
    check("ids like toString and __proto__ are parts like any other", "toString" in parts and "__proto__" in parts, parts)
    check("two parts under one id are both drawn, the second offset", "toString#2" in parts and script(
        "function x(k){return +document.querySelector('#bench-svg [data-part=\"'+k+'\"] .bn-body').getAttribute('x');} return x('toString#2') - x('toString');") > 0)
    check("and a note says so", any("2 parts are called toString" in n for n in script(NOTES)), script(NOTES))
    fitbox = script("var v=document.getElementById('bench-svg').getAttribute('viewBox').split(' ').map(Number),"
                    " f=document.querySelector('#bench-svg [data-part=\"far\"] .bn-body'), h=document.querySelector('#bench-svg [data-part=\"host\"] .bn-body');"
                    "function inside(r){var x=+r.getAttribute('x'), y=+r.getAttribute('y');return x>=v[0]&&y>=v[1]&&x+(+r.getAttribute('width'))<=v[0]+v[2]&&y+(+r.getAttribute('height'))<=v[1]+v[3];}"
                    "return [inside(f), inside(h), v];")
    check("Fit reaches a bench tens of thousands of units across", fitbox[0] and fitbox[1], fitbox)
    script("document.dispatchEvent(new CustomEvent('embarch:verdicts',{detail:{roles:{dut:{status:'pass',text:'\u25cf validated just now'}},boards:{}}}));")
    pill = script("var v=document.querySelector('#bench-svg [data-verdict=\"shield1\"] rect'), s=document.querySelector('#bench-svg [data-part=\"shield1\"] .bn-body'),"
                  " t=document.querySelector('#bench-svg [data-part=\"shield2\"] .bn-body'), b=document.querySelector('#bench-svg [data-part=\"base1\"] .bn-body');"
                  "var parts=document.querySelectorAll('#bench-svg [data-part]'), last=parts[parts.length-1];"
                  "return v ? [+v.getAttribute('y') + (+v.getAttribute('height')), +t.getAttribute('y'), +s.getAttribute('y'), +b.getAttribute('y'),"
                  " !!(last.compareDocumentPosition(v.parentNode) & Node.DOCUMENT_POSITION_FOLLOWING)] : null;")
    check("L6: a mounted part's verdict sits above the top of its stack, drawn after every part",
          pill and pill[0] <= pill[1] < pill[2] < pill[3] and pill[4], pill)
    grid = script("return ['u1','u5','u6','u12'].map(function(k){var r=document.querySelector('#bench-svg [data-part=\"'+k+'\"] .bn-body');return [+r.getAttribute('x'), +r.getAttribute('y')];});")
    check("parts with no position wrap into a grid under the rest",
          grid[0][1] == grid[1][1] and grid[2][1] > grid[0][1] and grid[2][0] == grid[0][0] and grid[3][1] > grid[2][1], grid)

    # --- a project switch drops the last report -----------------------------
    check("the Report button is there before the switch", script("return !document.getElementById('topo-report').hidden;"))
    script("document.getElementById('project-button').click();")
    time.sleep(0.4)
    script("document.getElementById('project-path').value=arguments[0]; document.getElementById('project-open').click();", [fw2])
    wait_for("return document.getElementById('topo-report').hidden;", 8)
    check("L2: after a project switch the Report button, the report and its error are gone",
          script("var r=document.getElementById('topo-validate-report'), e=document.getElementById('topo-validate-error');"
                 "return document.getElementById('topo-report').hidden && r.textContent === '' && e.textContent === '' && e.style.display === 'none';"))
    script("var d=document.getElementById('project-dialog'); if (d && d.style.display==='block') document.getElementById('project-cancel').click();")
    wait_for("return !!document.querySelector('#bench-svg [data-part]');", 8)

    # --- another tab -------------------------------------------------------
    click('[data-topo-panel="signals"]')
    script("document.querySelector('.nav-item[data-tab=\"debug\"]').click();")
    check("L7: leaving the Topology tab closes its panel", script("return document.getElementById('topo-panel-signals').hidden;"))
    script("document.getElementById('topo-panel-signals').hidden = false;")
    key("")
    check("L7: and Esc on another tab leaves Topology alone",
          not script("return document.getElementById('topo-panel-signals').hidden;"))
    script("document.getElementById('topo-panel-signals').hidden = true;"
           "document.querySelector('.nav-item[data-tab=\"topology\"]').click();")

    # --- phone width -------------------------------------------------------
    pick_bench("pd-bench-drp")
    rq("POST", b + "/window/rect", {"width": 480, "height": 900})
    time.sleep(0.6)
    click('[data-topo-panel="signals"]')
    ph = script("var p=document.getElementById('topo-panel-signals').getBoundingClientRect(), s=document.getElementById('topo-stage').getBoundingClientRect();"
                "return {pw: p.width, sw: s.width, left: p.left - s.left, hscroll: document.scrollingElement.scrollWidth - innerWidth};")
    check("at phone width a panel spans the canvas", ph["pw"] >= ph["sw"] - 14 and ph["left"] <= 8 and ph["hscroll"] <= 0, ph)
    if SHOTS:
        shots.append(shot("bench-tab-phone-panel.png"))
    key("")
    rq("POST", b + "/window/rect", {"width": 1500, "height": 950})
    time.sleep(0.6)
    click('[data-topo-panel="diagram"]')
    if SHOTS:
        shots.append(shot("bench-tab-dark-panel.png"))
    key("")

    check("no uncaught JS error or unhandled rejection, load included", script("return window.__errs;") == [], script("return window.__errs;"))

    # --- no bench routes ---------------------------------------------------
    MODE["bench"] = "404"
    open_page()
    state = wait_for("var s=document.getElementById('bench-state'); return !s.hidden && s.textContent.indexOf('not available')>=0 ? s.textContent : '';")
    check("a server without the bench routes gets one quiet line", "bench routes not available" in (state or ""), state)
    check("never an error banner, never an empty bench standing in for a real one",
          script("var c=document.getElementById('bench-card');"
                 "return document.getElementById('bench-state').className.indexOf('placeholder-note')>=0 &&"
                 " !c.querySelector('.sd-error:not([hidden])') && document.getElementById('bench-body').hidden &&"
                 " document.getElementById('bench-pick').hidden && !document.querySelector('#bench-svg [data-part]');"))
    check("no uncaught JS error on the page without them", script("return window.__errs;") == [], script("return window.__errs;"))
    MODE["bench"] = "fixture"
    check("bench.js holds no interval", "setInterval" not in (REPO / "assets" / "bench.js").read_text())
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

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
