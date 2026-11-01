"""The bench canvas on the Topology tab, in a real browser.

Phase 2 of embarch-bench-editor-proposal.md: a saved bench drawn read-only as
the hardware on the shelf, beside the old diagram. None of it is reachable
from `cargo test` — every part, port and cable is SVG that `bench.js` builds
from the bench routes' JSON, the reveal-on-hover is pointer state, and the
glow is a class a snapshot toggles without rebuilding anything.

**The bench routes are served from a fixture, through a proxy in front of the
real binary.** `tests/fixtures/bench/fixture.json` holds the five
`GET /api/bench/*` answers, keyed by route and shaped as the bench editor's
JSON contract; every other request — the page, the scripts, `/events` — goes
to the UI untouched, so the snapshot the glow reads is the UI's own poll of
the stub Core below. Switching the proxy to `404` is how the page meets a
server that has no bench routes at all.

Ports: geckodriver at EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, a desk
port nothing listens on at +1, the fixture proxy the browser opens at +2, and
the stub Core at EMBARCH_STUB_PORT (default +3). Screenshots of the canvas in
each theme go to EMBARCH_BENCH_SHOTS when it names a directory.

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

FIXTURE = json.loads((REPO / "tests" / "fixtures" / "bench" / "fixture.json").read_text())
PD = FIXTURE["/api/bench/benches/pd-bench-drp"]
VOCAB = FIXTURE["/api/bench/vocabulary"]
PARTNER_PROBE = PD["parts"][2]["usb"]["stlink"]["probe"]
DEVKIT_PROBE = "000683001234"

# The probes Core enumerates: the dev kit's from the start, the partner's
# added and taken away mid-run. The debug probe's serial is never here.
PROBES = [{"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": DEVKIT_PROBE}]
MODE = {"bench": "fixture"}


class Core(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype):
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
                   "/probes/enrolled": [], "/signals": [], "/serial-ports": [], "/alerts": [],
                   "/studies": [], "/logs/recent": {"lines": []}}
        if path in answers:
            return self._send(200, json.dumps(answers[path]).encode(), "application/json")
        return self._send(404, b"not found in the stub", "text/plain")


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
        length = int(self.headers.get("content-length") or 0)
        data = self.rfile.read(length) if length else None
        up = http.client.HTTPConnection("127.0.0.1", int(UI_PORT), timeout=120)
        headers = {k: v for k, v in self.headers.items()
                   if k.lower() not in ("connection", "keep-alive", "accept-encoding", "content-length")}
        try:
            up.request(self.command, self.path, body=data, headers=headers)
            resp = up.getresponse()
        except OSError as e:
            return self._send(502, str(e).encode(), "text/plain")
        self.send_response(resp.status)
        stream = (resp.getheader("content-type") or "").startswith("text/event-stream")
        for k, v in resp.getheaders():
            if k.lower() not in ("transfer-encoding", "connection", "content-length", "keep-alive"):
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
        if resp.status != 304:
            self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body and resp.status != 304:
            self.wfile.write(body)

    def do_GET(self):
        path = urlparse(self.path).path
        if path.startswith("/api/bench/"):
            if MODE["bench"] == "404" or path not in FIXTURE:
                return self._send(404, b"Not Found", "text/plain")
            return self._send(200, json.dumps(FIXTURE[path]).encode(), "application/json")
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


def open_page():
    rq("POST", b + "/url", {"url": "about:blank"})
    rq("POST", b + "/url", {"url": PAGE + "/#topology"})
    script("window.__errs=[];window.addEventListener('error',function(e){window.__errs.push(String(e.message))});")


def shot(name):
    if not SHOTS:
        return None
    os.makedirs(SHOTS, exist_ok=True)
    script("document.getElementById('bench-card').scrollIntoView({block:'start'});")
    time.sleep(0.4)
    png = rq("GET", b + "/element/" + element("#bench-card")[ELEMENT] + "/screenshot")
    path = os.path.join(SHOTS, name)
    with open(path, "wb") as f:
        f.write(base64.b64decode(png))
    return path


PORTS = ("return Array.from(document.querySelectorAll('#bench-svg [data-part=\"' + arguments[0] + '\"] [data-conn]'))"
         ".map(function(g){return [g.getAttribute('data-conn'), g.getAttribute('data-x'), g.getAttribute('data-y'),"
         " g.classList.contains('bn-free')];});")
CABLES = ("return Array.from(document.querySelectorAll('#bench-svg .bn-cable')).map(function(p){"
          "return [p.getAttribute('data-cable'), p.classList.contains('bn-cfg'), p.getAttribute('d')];});")

try:
    rq("POST", b + "/window/rect", {"width": 1500, "height": 1100})
    open_page()
    wait_for("return document.querySelectorAll('#bench-svg [data-part]').length > 0;")
    script("document.getElementById('bench-card').scrollIntoView({block:'start'});")
    time.sleep(0.3)

    # --- the frame ---------------------------------------------------------
    check("the bench canvas sits on the Topology tab, beside the old diagram",
          script("var t=document.querySelector('.tab-panel[data-tab=\"topology\"]');"
                 "return !!t.querySelector('#bench-card') && !!t.querySelector('#topology-diagram');"))
    check("bench.js is served by the binary and loaded by the page",
          script("return Array.from(document.scripts).some(function(s){return /\\/bench\\.js$/.test(s.src);});"))
    check("the picker lists the benches the route answered",
          script("return Array.from(document.getElementById('bench-pick').options).map(function(o){return o.value;});")
          == ["pd-bench-drp", "ble-dev-bench"])
    check("the path under the title is the bench's file",
          script("return document.getElementById('bench-path').textContent;") == "embarch/topologies/pd-bench-drp.toml")
    notes = script("return Array.from(document.querySelectorAll('#bench-notes li')).map(function(l){return l.textContent;});")
    check("a template file and a bench file the routes could not read are each named, not hidden",
          any("embarch/parts/bad.toml" in n and "unknown kind" in n for n in notes)
          and any("embarch/topologies/broken.toml" in n for n in notes), notes)

    # --- every look --------------------------------------------------------
    looks = script("return Array.from(document.querySelectorAll('#bench-svg [data-part]')).map(function(g){"
                   "return [g.getAttribute('data-part'), g.getAttribute('data-look')];});")
    drawn = dict(looks)
    check("every part on the bench is drawn, the template-missing one included",
          sorted(drawn) == sorted(p["id"] for p in PD["parts"]), sorted(drawn))
    check("every served look is drawn by its own look",
          set(VOCAB["looks"]) <= set(drawn.values()), sorted(set(drawn.values())))
    check("a look this build does not draw is drawn as a board, not dropped", drawn.get("odd") == "board", drawn.get("odd"))
    check("a part whose template is gone is drawn as a board that says so",
          drawn.get("ghost") == "board" and "template missing" in
          script("return document.querySelector('#bench-svg [data-part=\"ghost\"] .bn-sub').textContent;"))
    check("its cable still lands, on a connector named by the cable",
          script("return !!document.querySelector('#bench-svg [data-conn=\"ghost.j1\"].bn-port-missing');"))
    check("a kind this build does not draw is drawn as a plain port, not dropped",
          script("var g=document.querySelector('#bench-svg [data-conn=\"odd.usb\"]'); return !!g;") and
          script("var p=document.querySelector('#bench-svg [data-part=\"odd\"]');"
                 "return p.getAttribute('data-look');") == "board")
    check("a stacking connector is never drawn as a port: the shield sits on its board instead",
          not script("return !!document.querySelector('#bench-svg [data-conn=\"partner.arduino\"],"
                     " #bench-svg [data-conn=\"drp.arduino\"]');"))
    geo = script("function r(id){var g=document.querySelector('#bench-svg [data-part=\"'+id+'\"] .bn-body');"
                 "return [+g.getAttribute('x'), +g.getAttribute('y'), +g.getAttribute('height')];}"
                 "return [r('drp'), r('partner')];")
    check("the mounted shield is drawn on top of its board", geo[0][0] == geo[1][0] and
          abs(geo[0][1] + geo[0][2] - geo[1][1]) < 0.5, geo)
    check("the analyzer is a plain case: no vendor colours, two USB-C at the sides and USB on top",
          script("var p=document.querySelector('#bench-svg [data-part=\"cy4500\"]');"
                 "return p.querySelector('.bn-body').classList.contains('bn-case') &&"
                 " !p.querySelector('.bn-pcb, .bn-pcb-2');")
          and sorted((c[0], c[3]) for c in script(PORTS, ["cy4500"])) ==
          [("cy4500.in", False), ("cy4500.out", False), ("cy4500.usb", False)])

    # --- reveal on intent --------------------------------------------------
    before = script(PORTS, ["dut"])
    cables_before = script(CABLES)
    check("a part shows only its wired connectors",
          sorted(c[0] for c in before) == ["dut.swd", "dut.usb-c-1"] and not any(c[3] for c in before), before)
    move_to('#bench-svg [data-part="dut"] .bn-body')
    after = script(PORTS, ["dut"])
    check("hovering it shows its free ones too",
          sorted(c[0] for c in after) == ["dut.dc-in", "dut.swd", "dut.usb-c-1", "dut.usb-c-2"]
          and sorted(c[0] for c in after if c[3]) == ["dut.dc-in", "dut.usb-c-2"], after)
    check("and no wired connector moved", all(any(a[:3] == w[:3] for a in after) for w in before), (before, after))
    check("nor any cable", script(CABLES) == cables_before)
    move_to('#bench-svg [data-part="drp"] .bn-body')
    drp = script(PORTS, ["drp"])
    check("a mounted shield reveals its free row without moving its wired ones",
          sorted(c[0] for c in drp) == ["drp.cn2", "drp.cn3", "drp.usb-c"] and script(CABLES) == cables_before, drp)
    move_to("#bench-pick")
    check("leaving the part hides them again", sorted(c[0] for c in script(PORTS, ["dut"])) == ["dut.swd", "dut.usb-c-1"])

    # --- the hub -----------------------------------------------------------
    hub = script("return Array.from(document.querySelectorAll('#bench-svg [data-part=\"host\"] [data-hub-port]'))"
                 ".map(function(g){return g.querySelector('title').textContent;});")
    hub_labels = script("var out=[];document.querySelectorAll('#bench-svg [data-part=\"host\"] .bn-clabel')"
                        ".forEach(function(t){out.push(t.textContent);});return out;")
    to_host = [c for c in PD["cables"] if c["to"] == "host" or c["from"] == "host"]
    check("this machine has one port per cable that reaches it", len(hub) == len(to_host), (len(hub), hub))
    check("each named by the part it goes to",
          {"pd-bench", "CY4500", "eload", "nRF54L15DK", "aux board", "Odd widget"} <= set(hub_labels), hub_labels)
    check("a probe's port named by what its SWD cable reaches", "Debug probe → DUT" in hub_labels, hub_labels)
    ys = script("return Array.from(document.querySelectorAll('#bench-svg [data-part=\"host\"] [data-hub-port]'))"
                ".map(function(g){return +g.getAttribute('data-y');});")
    check("ordered top to bottom by the part at the far end, so the cables do not cross",
          hub_labels[0] == "Debug probe → DUT" and ys == sorted(ys), hub_labels)

    # --- bench-level solid, config dashed; config tabs ---------------------
    cfgs = PD["configs"]
    tabs = script("return Array.from(document.querySelectorAll('#bench-configs [data-cfg]')).map(function(t){"
                  "return [t.textContent, t.getAttribute('aria-selected')];});")
    check("a tab per config, the first selected", tabs == [[c["name"], "true" if i == 0 else "false"] for i, c in enumerate(cfgs)], tabs)
    cab = script(CABLES)
    check("bench-level cables are solid, one per cable to this machine",
          len([c for c in cab if not c[1]]) == len(PD["cables"]), cab)
    check("config cables are dashed, one per cable in the config on screen",
          len([c for c in cab if c[1]]) == len(cfgs[0]["cables"]), cab)
    solid_a = sorted(c[2] for c in cab if not c[1])
    script("document.querySelector('#bench-configs [data-cfg=\"1\"]').click();")
    cab_b = script(CABLES)
    check("a tab switches which config's dashed cables are drawn",
          len([c for c in cab_b if c[1]]) == len(cfgs[1]["cables"]) - 1
          and script("return !!document.querySelector('#bench-svg [data-conn=\"psu.out\"]') &&"
                     " !document.querySelector('#bench-svg [data-conn=\"drp.cn2\"]');"), cab_b)
    check("and leaves the bench-level ones where they were",
          sorted(c[2] for c in cab_b if not c[1]) == solid_a)
    check("a config cable to a part the bench lacks is named, not drawn",
          any("nowhere" in n for n in script("return Array.from(document.querySelectorAll('#bench-notes li'))"
                                             ".map(function(l){return l.textContent;});")))
    script("document.querySelector('#bench-configs [data-cfg=\"0\"]').click();")
    check("switching back restores the first config's cables", script(CABLES) == cab)

    # --- select, inspect ---------------------------------------------------
    click('#bench-svg [data-part="partner"] .bn-body')
    ins = script("return document.getElementById('bench-inspector').innerText;")
    check("selecting a part opens a read-only inspector: template, role, connectors, what each cable reaches",
          "pd-bench" in ins and "NUCLEO-G0B1RE" in ins and "built in" in ins and "Role" in ins
          and "ST-LINK USB" in ins and "This machine" in ins and "X-NUCLEO-DRP1M1" in ins
          and "probe " + PARTNER_PROBE in ins, ins)
    check("the inspector has no control that writes",
          script("return document.querySelectorAll('#bench-inspector button, #bench-inspector input,"
                 " #bench-inspector select').length;") == 0)
    click('#bench-svg [data-part="dut"] [data-conn="dut.usb-c-1"]')
    ins = script("return document.getElementById('bench-inspector').innerText;")
    check("a wired connector selects its cable, which says its ends and its config",
          "DUT · USB-C 1" in ins and "CY4500 · USB-C" in ins and cfgs[0]["name"] in ins, ins)
    click('#bench-svg [data-part="eload"] .bn-body')
    move_to("#bench-pick")

    # --- the live glow, and a snapshot that rebuilds nothing ---------------
    LIVE = "return Array.from(document.querySelectorAll('#bench-svg .bn-live')).map(function(e){return e.getAttribute('data-probe');});"
    live = wait_for("var l=document.querySelectorAll('#bench-svg .bn-live'); return l.length ? true : false;", 12)
    first = script(LIVE)
    check("a usb-host connector whose bound probe is enumerated glows, with its cable",
          set(first) == {DEVKIT_PROBE} and len(first) >= 3, first)
    check("a bound probe Core does not enumerate does not", "000683009999" not in first)
    click('#bench-svg [data-part="partner"] .bn-body')
    move_to("#bench-pick")
    script("window.__benchNode=document.querySelector('#bench-svg [data-part=\"partner\"]');"
           "window.__benchSvg=document.querySelector('#bench-svg').firstElementChild;")
    PROBES.append({"identifier": "STLink", "vendor_id": 1155, "product_id": 14158, "serial_number": PARTNER_PROBE})
    lit = wait_for("return !!document.querySelector('#bench-svg [data-probe=\"%s\"].bn-live');" % PARTNER_PROBE, 14)
    check("a snapshot that enumerates the partner's probe lights its connector", lit)
    check("without rebuilding the canvas",
          script("return document.querySelector('#bench-svg [data-part=\"partner\"]') === window.__benchNode &&"
                 " document.querySelector('#bench-svg').firstElementChild === window.__benchSvg;"))
    check("so the selection survives it",
          script("return window.__benchNode.classList.contains('bn-sel') &&"
                 " document.getElementById('bench-inspector').innerText.indexOf('pd-bench') === 0;"))
    check("and the inspector says the bound probe is enumerated",
          script("var r=document.querySelector('#bench-inspector [data-probe]');"
                 "return !!r && r.classList.contains('bn-live');"))
    shot_dark = shot("bench-canvas-dark.png")
    script("document.documentElement.setAttribute('data-theme','light');")
    shot_light = shot("bench-canvas-light.png")
    script("document.documentElement.setAttribute('data-theme','dark');")
    PROBES.pop()
    dark = wait_for("return !document.querySelector('#bench-svg [data-probe=\"%s\"].bn-live');" % PARTNER_PROBE, 14)
    check("a snapshot that no longer has it puts the glow out, still rebuilding nothing",
          dark and script("return document.querySelector('#bench-svg [data-part=\"partner\"]') === window.__benchNode &&"
                          " window.__benchNode.classList.contains('bn-sel');"))

    # --- pan, zoom, fit ----------------------------------------------------
    vb = lambda: [float(v) for v in script("return document.getElementById('bench-svg').getAttribute('viewBox');").split()]
    v0 = vb()
    rq("POST", b + "/actions", {"actions": [{"type": "wheel", "id": "wheel", "actions": [
        {"type": "scroll", "x": 0, "y": 0, "deltaX": 0, "deltaY": -240, "origin": element("#bench-svg")}]}]})
    rq("DELETE", b + "/actions")
    v1 = vb()
    check("the wheel zooms in", v1[2] < v0[2], (v0, v1))
    # A point on the bare grid, and one the drag ends on, both clear of every
    # part and cable, as offsets from the canvas's centre.
    empty = script("var w=document.getElementById('bench-wrap').getBoundingClientRect();"
                   "function bare(x,y){var e=document.elementFromPoint(x,y);return !!e&&(e.id==='bench-grid'||e.id==='bench-svg');}"
                   "for(var y=w.top+60;y<w.bottom-100;y+=15)for(var x=w.left+60;x<w.right-200;x+=15)"
                   "if(bare(x,y)&&bare(x+120,y+40))return [Math.round(x-(w.left+w.width/2)),Math.round(y-(w.top+w.height/2))];"
                   "return null;")
    check("the canvas has bare grid to pan from", empty is not None)
    pointer([{"type": "pointerMove", "duration": 0, "origin": element("#bench-wrap"), "x": empty[0], "y": empty[1]},
             {"type": "pointerDown", "button": 0},
             {"type": "pointerMove", "duration": 100, "origin": "pointer", "x": 120, "y": 40},
             {"type": "pointerUp", "button": 0}])
    v2 = vb()
    check("dragging empty space pans", v2[0] < v1[0] and v2[1] < v1[1] and abs(v2[2] - v1[2]) < 0.01, (v1, v2))
    check("and keeps the selection", script("return !!document.querySelector('#bench-svg [data-part=\"partner\"].bn-sel');"))
    script("document.getElementById('bench-fit').click();")
    check("Fit brings the whole bench back", vb() == v0, (v0, vb()))

    # --- another bench -----------------------------------------------------
    script("var s=document.getElementById('bench-pick'); s.value='ble-dev-bench'; s.dispatchEvent(new Event('change'));")
    wait_for("return !!document.querySelector('#bench-svg [data-part=\"dev-bench\"]');")
    check("the picker loads another bench, and a legacy file says how it was laid out",
          "no [[parts]]" in script("return document.getElementById('bench-path').textContent;"))
    flip = script("var a=document.querySelector('#bench-svg [data-conn=\"dut.radio\"]'),"
                  " p=document.querySelector('#bench-svg [data-part=\"dut\"] .bn-body');"
                  "return [+a.getAttribute('data-x'), +p.getAttribute('x')];")
    check("a flipped part draws its right-side connector on the left", flip[0] == flip[1], flip)

    check("no uncaught JS error while the bench was on screen", script("return window.__errs;") == [],
          script("return window.__errs;"))

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
    MODE["bench"] = "fixture"

    check("bench.js holds no interval", "setInterval" not in (REPO / "assets" / "bench.js").read_text())
    check("no uncaught JS error on the page without them", script("return window.__errs;") == [],
          script("return window.__errs;"))
    if shot_dark:
        print("screenshots: %s %s" % (shot_dark, shot_light))
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
