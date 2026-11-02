"""Editing a bench on the Topology tab's canvas, in a real browser.

Phase 3 of embarch-bench-editor-proposal.md: the parts bin, a cable laid by
click-then-click and by drag with the server's verdict on every end, a
refusal said in its sentence, delete and undo, a shield stacked and lifted,
a probe and serial ports bound from the tray, the inspector's edits, the
checks, an explicit Save and the `409` that follows a hand edit on disk, the
unsaved-changes prompt, a new bench, and the config "A" a first part-to-part
cable creates.

**Against the real bench routes, never fixtures**: the UI opens a scratch
firmware repo holding `embarch/topologies/` and `embarch/parts/`, every
verdict comes from the binary's own `POST /api/bench/candidates`, and every
save is checked in the TOML file on disk. A stub Core supplies the snapshot's
probes and serial ports (the tray), and a pass-through proxy in front of the
UI puts an error listener at the top of the page, so an error during load
counts too.

Ports: geckodriver at EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, the
proxy the browser opens at +1, the stub Core at +2 and a desk port nothing
listens on at +3. Screenshots go to EMBARCH_BENCH_SHOTS when it names a
directory, in both themes.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_bench_edit.py
"""
import base64, http.client, json, os, shutil, subprocess, sys, tempfile, threading, time, tomllib, urllib.request
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

REPO = Path(__file__).resolve().parents[2]
TOKEN = "stub-token"
UI_PORT = addr.ui_port(4881)
BASE = addr.gecko_url()
PROXY_PORT = int(UI_PORT) + 1
CORE_PORT = int(UI_PORT) + 2
DESK_PORT = int(UI_PORT) + 3
PAGE = "http://127.0.0.1:%d" % PROXY_PORT
SHOTS = os.environ.get("EMBARCH_BENCH_SHOTS")

BOUND = "PRB-BOUND-0001"
LOOSE = "PRB-LOOSE-0002"
PROBES = [{"identifier": "ST-LINK/V3", "vendor_id": 1155, "product_id": 14164, "serial_number": BOUND},
          {"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": LOOSE}]
SERIAL_PORTS = [
    # one alone under its serial
    {"port_name": "COM7", "detected_by": "enumerated", "vendor_id": 1027, "product_id": 24577, "serial_number": "FT7ONLY",
     "product": None, "interface": 0},
    # two siblings sharing one serial
    {"port_name": "COM8", "detected_by": "enumerated", "vendor_id": 1027, "product_id": 24592, "serial_number": "DUAL55",
     "product": None, "interface": 0},
    {"port_name": "COM9", "detected_by": "enumerated", "vendor_id": 1027, "product_id": 24592, "serial_number": "DUAL55",
     "product": None, "interface": 2},
    # a bridge with no serial at all
    {"port_name": "COM10", "detected_by": "enumerated", "vendor_id": 6790, "product_id": 29987, "serial_number": None,
     "product": None, "interface": None},
]

PD_DUT = """id = "pd-dut"
label = "PD DUT"
category = "board"
look = "power-board"
[[connectors]]
id = "swd"
label = "SWD"
kind = "swd"
side = "left"
[[connectors]]
id = "usb-c-1"
label = "USB-C 1"
kind = "usb-c"
side = "right"
power = "source"
[[connectors]]
id = "usb-c-2"
label = "USB-C 2"
kind = "usb-c"
side = "right"
power = "drp"
"""

# A board whose USB carries three serial ports, so a dropped port asks which.
DFU = """id = "dfu-board"
label = "DFU board"
category = "board"
look = "board"
[[connectors]]
id = "usb"
label = "USB"
kind = "usb-host"
side = "left"
functions = [{ function = "vcom" }, { function = "bootload-app" }, { function = "bootload-bootloader" }]
"""

EDIT_BENCH = """name = "edit-bench"
saved_at_utc_ms = 3

[[parts]]
id = "host"
template = "this-machine"
at = [20, 110]

[[parts]]
id = "partner"
template = "nucleo-g0b1re"
label = "pd-bench"
at = [330, 130]
usb = { stlink = { probe = "%s" } }

[[parts]]
id = "drp"
template = "x-nucleo-drp1m1"
at = [330, 560]

[[parts]]
id = "dut"
template = "pd-dut"
role = "dut"
at = [700, 110]

[[parts]]
id = "an"
template = "cy4500"
at = [700, 380]

[[parts]]
id = "dfu"
template = "dfu-board"
at = [20, 400]

[[cables]]
from = "partner.stlink"
to = "host"

[[configs]]
name = "A"
cables = [["dut.usb-c-1", "an.in"]]

[[configs]]
name = "B"
cables = [["dut.usb-c-2", "an.out"]]
""" % BOUND

OTHER_BENCH = """name = "other-bench"
saved_at_utc_ms = 2
parts = [{ id = "host", template = "this-machine", at = [20, 110] }]
"""

LOOSE_BENCH = """name = "loose-bench"
saved_at_utc_ms = 1

[[parts]]
id = "host"
template = "this-machine"
at = [20, 110]

[[parts]]
id = "dut"
template = "pd-dut"
at = [360, 110]

[[parts]]
id = "an"
template = "cy4500"
at = [700, 110]
"""


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
                   "/probes/enrolled": [], "/signals": [], "/serial-ports": SERIAL_PORTS, "/alerts": [],
                   "/studies": [], "/logs/recent": {"lines": []}}
        if path in answers:
            return self._send(200, answers[path])
        return self._send(404, b"not found in the stub", "text/plain")

    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length") or 0))
        CORE_WRITES.append(urlparse(self.path).path)
        return self._send(404, b"not found in the stub", "text/plain")

    do_PUT = do_DELETE = do_POST


CORE_WRITES = []
LISTEN = (b"<script>window.__errs=[];addEventListener('error',function(e){__errs.push(String(e.message))});"
          b"addEventListener('unhandledrejection',function(e){__errs.push('unhandled: '+String(e.reason&&e.reason.message||e.reason))});</script>")


class Proxy(BaseHTTPRequestHandler):
    """Everything to the UI, the bench routes included; the page gets the error listener first."""
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

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
            body = str(e).encode()
            self.send_response(502)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
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

iso = tempfile.mkdtemp(prefix="embarch-ui-bench-edit-")
fw = Path(iso) / "fw"
(fw / ".git").mkdir(parents=True)
(fw / "embarch" / "topologies").mkdir(parents=True)
(fw / "embarch" / "parts").mkdir(parents=True)
(fw / "embarch" / "parts" / "pd-dut.toml").write_text(PD_DUT)
(fw / "embarch" / "parts" / "dfu-board.toml").write_text(DFU)
TOPO = fw / "embarch" / "topologies"
(TOPO / "edit-bench.toml").write_text(EDIT_BENCH)
(TOPO / "other-bench.toml").write_text(OTHER_BENCH)
(TOPO / "loose-bench.toml").write_text(LOOSE_BENCH)
cfg = Path(iso) / "ui.toml"
cfg.write_text('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n\n[study_designer]\nfirmware_repo_path = "%s"\n'
               % (CORE_PORT, TOKEN, fw))
env = dict(os.environ, EMBARCH_UI_CONFIG=str(cfg), EMBARCH_UI_PORT=UI_PORT, EMBARCH_DESK_PORT=str(DESK_PORT),
           EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data", XDG_CONFIG_HOME=iso + "/config",
           EMBARCH_UI_STATE=iso + "/recent-projects.json", HOME=iso + "/home")
ui = subprocess.Popen([os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)

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
        time.sleep(0.15)
    return script(js, args)


def element(css):
    return rq("POST", b + "/element", {"using": "css selector", "value": css})


def pointer(actions):
    rq("POST", b + "/actions", {"actions": [{"type": "pointer", "id": "mouse",
                                             "parameters": {"pointerType": "mouse"}, "actions": actions}]})
    rq("DELETE", b + "/actions")


def click(css, x=0, y=0):
    pointer([{"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y},
             {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}])


def move_to(css, x=0, y=0):
    pointer([{"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y}])


def center(css):
    return script("var r=document.querySelector(arguments[0]).getBoundingClientRect(); return [r.left + r.width/2, r.top + r.height/2];", [css])


def where(dst):
    return center(dst) if isinstance(dst, str) else dst


def press_drag(src_css, dst, steps=8):
    """Press on src and travel to dst (a selector or viewport [x, y]), the button still down."""
    sx, sy = center(src_css)
    dx, dy = where(dst)
    acts = [{"type": "pointerMove", "duration": 0, "origin": "viewport", "x": int(sx), "y": int(sy)},
            {"type": "pointerDown", "button": 0}]
    for i in range(1, steps + 1):
        acts.append({"type": "pointerMove", "duration": 30, "origin": "viewport",
                     "x": int(sx + (dx - sx) * i / steps), "y": int(sy + (dy - sy) * i / steps)})
    rq("POST", b + "/actions", {"actions": [{"type": "pointer", "id": "mouse",
                                             "parameters": {"pointerType": "mouse"}, "actions": acts}]})


def release_at(dst):
    """Travel on to dst (looked up now) and let go."""
    dx, dy = where(dst)
    pointer([{"type": "pointerMove", "duration": 60, "origin": "viewport", "x": int(dx), "y": int(dy)},
             {"type": "pointerUp", "button": 0}])


def drag_to(src_css, dst, steps=8):
    press_drag(src_css, dst, steps)
    release_at(dst)


def screen_of(wx, wy):
    """A point on the bench, in the viewport."""
    return script("var m=document.getElementById('bench-svg').getScreenCTM(); return [m.a*arguments[0]+m.e, m.d*arguments[1]+m.f];", [wx, wy])


def key(value, mods=()):
    acts = [{"type": "keyDown", "value": m} for m in mods] + [{"type": "keyDown", "value": value}, {"type": "keyUp", "value": value}] + \
        [{"type": "keyUp", "value": m} for m in mods]
    rq("POST", b + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": acts}]})
    rq("DELETE", b + "/actions")


CTRL = ""
SHIFT = ""
ESC = ""
DEL = ""


# A synthetic HTML5 drag: what a person dragging a card or a tray chip does,
# event for event, with one DataTransfer carried through.
DND = """
var src = document.querySelector(arguments[0]), x = arguments[1], y = arguments[2];
var dt = new DataTransfer();
function fire(el, type) {
  var ev = new DragEvent(type, {bubbles: true, cancelable: true, dataTransfer: dt, clientX: x, clientY: y});
  el.dispatchEvent(ev);
  return ev;
}
fire(src, 'dragstart');
fire(document.elementFromPoint(x, y), 'dragover');
var over = fire(document.elementFromPoint(x, y), 'dragover');
var allowed = over.defaultPrevented;
if (allowed) fire(document.elementFromPoint(x, y), 'drop');
fire(src, 'dragend');
return allowed;
"""


def dnd(src_css, x, y):
    return script(DND, [src_css, x, y])


def open_page():
    rq("POST", b + "/url", {"url": "about:blank"})
    rq("POST", b + "/url", {"url": PAGE + "/#topology"})


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


def file(slug):
    return tomllib.loads((TOPO / (slug + ".toml")).read_text())


def part_of(doc, pid):
    return next((p for p in doc.get("parts", []) if p.get("id") == pid), None)


def pick_bench(slug):
    script("var s=document.getElementById('bench-pick'); s.value=arguments[0]; s.dispatchEvent(new Event('change'));", [slug])


def on_bench(slug):
    return wait_for("return document.getElementById('bench-path').textContent.indexOf(arguments[0] + '.toml') >= 0 &&"
                    " !!document.querySelector('#bench-svg [data-part]');", 8, [slug])


def sits_on(top, base):
    """Whether part `top` is drawn sitting on part `base`: same left edge, its bottom on the base's top."""
    g = script("var a=document.querySelector('#bench-svg [data-part=\"' + arguments[0] + '\"] .bn-body').getBoundingClientRect(),"
               " c=document.querySelector('#bench-svg [data-part=\"' + arguments[1] + '\"] .bn-body').getBoundingClientRect();"
               "return [a.left - c.left, a.bottom - c.top];", [top, base])
    return abs(g[0]) <= 3 and abs(g[1]) <= 3   # a selected part's thicker stroke counts in its box


def checks_done():
    return wait_for("return document.getElementById('bench-checks-btn').getAttribute('data-state') === 'ok';", 8)


def dialog_open():
    return script("return document.getElementById('bench-dialog').style.display === 'block';")


def dialog_text():
    return script("return document.getElementById('bench-dialog').innerText;")


def select_part(pid):
    # A little left of centre: clear of a chip drawn on the body's quiet side.
    w = script("return document.querySelector(arguments[0]).getBoundingClientRect().width;", ['#bench-svg [data-part="%s"] .bn-body' % pid])
    click('#bench-svg [data-part="%s"] .bn-body' % pid, -int(w / 5), 4)


def conn(pid, cid):
    return '#bench-svg [data-conn="%s.%s"]' % (pid, cid)


def classes(css):
    return script("var e=document.querySelector(arguments[0]); return e ? e.getAttribute('class') : null;", [css])


def dirty():
    return script("return !document.getElementById('bench-dirty').hidden;")


def notice():
    return script("var n=document.getElementById('bench-notice'); return n.hidden ? '' : n.textContent;")


def cables_shown():
    return sorted(script("return Array.from(document.querySelectorAll('#bench-svg .bn-cable')).map(function(p){return p.getAttribute('data-ends');});"))


def undo():
    key("z", (CTRL,))


def redo():
    key("z", (CTRL, SHIFT))


def save():
    key("s", (CTRL,))


shots = []
try:
    rq("POST", b + "/window/rect", {"width": 1900, "height": 1000})
    open_page()
    on_bench("edit-bench")
    check("the open repo's newest bench is drawn", script("return document.getElementById('bench-pick').value;") == "edit-bench")
    check("the bar holds + Part, undo, redo and Save, with nothing to save yet",
          script("return !document.getElementById('bench-edit').hidden && document.getElementById('bench-save').disabled &&"
                 " document.getElementById('bench-undo').disabled && document.getElementById('bench-redo').disabled &&"
                 " document.getElementById('bench-dirty').hidden;"))
    checks_done()
    check("the Checks button counts what the check route said about the bench on load",
          script("var b=document.getElementById('bench-checks-btn'); return !b.hidden && /^\\d+$/.test(document.getElementById('bench-checks-count').textContent);"),
          script("return document.getElementById('bench-checks-count').textContent;"))
    check("New bench… is the picker's last entry", script(
        "var o=document.getElementById('bench-pick').options; return o[o.length-1].textContent;") == "New bench…")

    # --- the parts bin ------------------------------------------------------
    click("#bench-part-btn")
    check("+ Part opens the parts bin", script("return !document.getElementById('bench-bin').hidden;"))
    cards = script("return Array.from(document.querySelectorAll('#bench-bin [data-template]')).map(function(c){return c.getAttribute('data-template');});")
    check("it lists the built-ins and the repo's own templates, from /api/bench/parts",
          "debug-probe" in cards and "this-machine" in cards and "pd-dut" in cards and "dfu-board" in cards, cards)
    check("a repo template carries an Edit button, a built-in none",
          script("return !!document.querySelector('#bench-bin [data-template=\"pd-dut\"] [data-edit-part=\"pd-dut\"]') &&"
                 " !document.querySelector('#bench-bin [data-template=\"debug-probe\"] [data-edit-part]');"))
    script("window.__edits=[]; document.addEventListener('embarch:edit-part', function(e){ __edits.push(e.detail); });")
    script("document.querySelector('#bench-bin [data-edit-part=\"pd-dut\"]').click();"
           "document.querySelector('#bench-bin .bench-bin-new').click();")
    check("Edit and New part… dispatch embarch:edit-part for the part editor, with the id or null",
          script("return window.__edits;") == [{"id": "pd-dut"}, {"id": None}], script("return window.__edits;"))
    check("and neither places a part", not dirty())
    click('#bench-bin [data-template="debug-probe"]')
    placed = wait_for("return !!document.querySelector('#bench-svg [data-part=\"debug-probe\"]');", 4)
    check("clicking a card places the part near the view's centre, its id the template's", placed)
    check("the new part is selected, and the bench has unsaved changes",
          script("return document.querySelector('#bench-svg [data-part=\"debug-probe\"]').classList.contains('bn-sel');") and dirty() and
          not script("return document.getElementById('bench-save').disabled;"))
    click('#bench-bin [data-template="debug-probe"]')
    check("a second one gets the next free id", wait_for("return !!document.querySelector('#bench-svg [data-part=\"debug-probe-2\"]');", 4))
    undo()
    check("Ctrl+Z takes the second back", wait_for("return !document.querySelector('#bench-svg [data-part=\"debug-probe-2\"]');", 4))
    redo()
    check("Ctrl+Shift+Z puts it back", wait_for("return !!document.querySelector('#bench-svg [data-part=\"debug-probe-2\"]');", 4))
    undo()
    wait_for("return !document.querySelector('#bench-svg [data-part=\"debug-probe-2\"]');", 4)
    # Dragged from the bin onto the canvas, at a point: there it lands.
    gx, gy = script("var r=document.querySelector('#bench-svg [data-part=\"drp\"] .bn-body').getBoundingClientRect(); return [r.right + 170, r.top + 30];")
    dropped = dnd('#bench-bin [data-template="usb-uart-bridge"]', gx, gy)
    check("a card dragged onto the canvas is accepted there", dropped)
    check("and the part lands where it was dropped", wait_for(
        "var g=document.querySelector('#bench-svg [data-part=\"usb-uart-bridge\"] .bn-body'); if(!g) return false;"
        "var r=g.getBoundingClientRect(); return Math.abs(r.left + r.width/2 - arguments[0]) < 30 && Math.abs(r.top - arguments[1]) < 60;",
        4, [gx, gy]))
    # previewTemplate draws with the canvas's renderer and touches nothing it holds.
    pv = script("var before=document.getElementById('bench-svg').innerHTML;"
                "var s=document.createElementNS('http://www.w3.org/2000/svg','svg'); document.body.appendChild(s);"
                "window.embarchBench.previewTemplate(s, {id:'trial', label:'Trial', category:'board', look:'nucleo64', width:200,"
                " connectors:[{id:'u', label:'USB', kind:'usb-host', side:'left', functions:[{function:'probe'}]},"
                "{id:'c', label:'USB-C', kind:'usb-c', side:'right', power:'sink'}, {id:'h', label:'Headers', kind:'stack-socket', side:'top'}]});"
                "var out={part: !!s.querySelector('[data-part=\"trial\"][data-look=\"nucleo64\"]'), ports: s.querySelectorAll('[data-conn]').length,"
                " vb: s.getAttribute('viewBox'), cls: s.classList.contains('bn-svg'), same: before === document.getElementById('bench-svg').innerHTML,"
                " junk: (function(){ try { window.embarchBench.previewTemplate(s, {connectors:[null, 3, {}]}); return 'ok'; } catch (e) { return String(e); } })()};"
                "s.remove(); return out;")
    check("previewTemplate draws one template with the canvas's renderer, every cabled connector shown and the stack socket not",
          pv["part"] and pv["ports"] == 2 and pv["cls"] and pv["vb"], pv)
    check("as a pure draw: the canvas is untouched, and a half-typed template does not throw", pv["same"] and pv["junk"] == "ok", pv)
    # The part editor saved a template: the bin and the canvas re-read the catalog.
    (fw / "embarch" / "parts" / "probe-pod.toml").write_text(
        'id = "probe-pod"\nlabel = "Probe pod"\ncategory = "probe"\nlook = "pod"\n'
        '[[connectors]]\nid = "usb"\nlabel = "USB"\nkind = "usb-host"\nside = "left"\nfunctions = [{ function = "probe" }]\n')
    script("document.dispatchEvent(new CustomEvent('embarch:parts-changed'));")
    check("embarch:parts-changed re-reads the catalog into the bin, edits on screen kept",
          wait_for("return !!document.querySelector('#bench-bin [data-template=\"probe-pod\"]');", 4) and dirty() and
          script("return !!document.querySelector('#bench-svg [data-part=\"usb-uart-bridge\"]');"))
    click("#bench-bin-close")
    check("the bin's × closes it", script("return document.getElementById('bench-bin').hidden;"))

    # --- a cable, click then click -------------------------------------------
    select_part("debug-probe")
    check("a selected part shows its free connectors", script("return !!document.querySelector('" + conn("debug-probe", "swd") + "');"))
    click(conn("debug-probe", "swd"))
    ok = wait_for("var e=document.querySelector(arguments[0]); return !!e && e.classList.contains('bn-ok');", 6, [conn("dut", "swd")])
    check("pressing a free connector asks the server: the DUT's free SWD pulses as a fit, though nothing hovered it", ok)
    check("an end of another kind is not drawn at all", not script("return !!document.querySelector(arguments[0]);", [conn("dut", "usb-c-2")]) or
          "bn-dim" in (classes(conn("dut", "usb-c-2")) or ""))
    check("the hint says how to finish", "pulsing" in script("return document.getElementById('bench-hint').textContent;"))
    key(ESC)
    check("Esc cancels the cable being laid", wait_for("return !document.querySelector('#bench-svg .bn-ok, #bench-svg .bn-source');", 4))
    select_part("debug-probe")
    click(conn("debug-probe", "swd"))
    wait_for("var e=document.querySelector(arguments[0]); return !!e && e.classList.contains('bn-ok');", 6, [conn("dut", "swd")])
    click(conn("dut", "swd"))
    laid = wait_for("return document.querySelectorAll('#bench-svg .bn-cable[data-ends=\"debug-probe.swd|dut.swd\"]').length === 1;", 4)
    check("clicking the fitting end lays the cable", laid)
    check("in the config on screen, dashed", "bn-cfg" in (classes('#bench-svg .bn-cable[data-ends="debug-probe.swd|dut.swd"]') or ""))
    check("and selects it", script("return document.getElementById('bench-inspector').innerText.indexOf('swd cable') >= 0;"))

    # --- a refusal, in its sentence ----------------------------------------------
    select_part("dut")
    click(conn("dut", "usb-c-2"))
    ref = wait_for("var e=document.querySelector(arguments[0]); return !!e && e.classList.contains('bn-refused') ? e.getAttribute('data-why') : '';", 6, [conn("an", "in")])
    check("a refused end is drawn red with the server's sentence", "already goes to" in (ref or ""), ref)
    check("and a fitting one beside it pulses", "bn-ok" in (classes(conn("an", "out")) or ""))
    move_to(conn("an", "in"))
    why = wait_for("var w=document.querySelector('#bench-svg [data-why-box]'); return w ? w.textContent : '';", 4)
    check("hovering the refused end puts its sentence beside the pointer", "already goes to" in (why or ""), why)
    if SHOTS:
        theme("dark"); shot("edit-laying-dark.png")
        theme("light"); shot("edit-laying-light.png")
        theme("dark")
    click(conn("an", "in"))
    check("clicking it lays nothing and says why", "already goes to" in notice() and
          not script("return !!document.querySelector('#bench-svg .bn-cable[data-ends=\"an.in|dut.usb-c-2\"]');"), notice())

    # --- a cable by drag, to the hub's new-USB slot ---------------------------------
    select_part("debug-probe")
    press_drag(conn("debug-probe", "usb"), "#bench-svg [data-part=\"host\"] .bn-body")
    slot = wait_for("return !!document.querySelector('#bench-svg [data-part=\"host\"] [data-slot].bn-ok');", 6)
    check("dragging from a usb-host connector shows the hub's new-USB slot, pulsing", slot)
    release_at('#bench-svg [data-part="host"] [data-slot]')
    hub = wait_for("return document.querySelectorAll('#bench-svg .bn-cable.bn-bench[data-ends=\"debug-probe.usb|host\"]').length === 1;", 4)
    check("releasing on it lays a bench-level cable to this machine", hub)
    check("and this machine names the new port after the part", "Debug probe" in script(
        "return Array.from(document.querySelectorAll('#bench-svg [data-part=\"host\"] .bn-clabel')).map(function(t){return t.textContent;}).join('|');"))
    select_part("usb-uart-bridge")
    drag_to(conn("usb-uart-bridge", "usb"), '#bench-svg [data-part="an"] .bn-body')
    check("a drag released on an end of another kind lays nothing",
          not script("return !!document.querySelector('#bench-svg .bn-cable[data-ends*=\"usb-uart-bridge\"]');"))

    # --- delete and undo -------------------------------------------------------
    click(conn("debug-probe", "swd"))
    check("a wired connector starts nothing: clicking it selects its cable", script("return document.getElementById('bench-inspector').innerText.indexOf('swd cable') >= 0;"))
    key(DEL)
    check("Del deletes the selected cable", wait_for("return !document.querySelector('#bench-svg .bn-cable[data-ends=\"debug-probe.swd|dut.swd\"]');", 4))
    undo()
    check("and Ctrl+Z brings it back", wait_for("return !!document.querySelector('#bench-svg .bn-cable[data-ends=\"debug-probe.swd|dut.swd\"]');", 4))
    # A part with a cable in each config: deleting it takes them all.
    script("document.querySelector('#bench-configs [data-cfg=\"1\"]').click();")
    wait_for("return !!document.querySelector('#bench-svg .bn-cable[data-ends=\"an.out|dut.usb-c-2\"]');", 4)
    select_part("an")
    key(DEL)
    gone_b = wait_for("return !document.querySelector('#bench-svg [data-part=\"an\"]');", 4)
    script("document.querySelector('#bench-configs [data-cfg=\"0\"]').click();")
    check("Del on a part deletes it with its cables in every config", gone_b and wait_for(
        "return !document.querySelector('#bench-svg .bn-cable[data-ends*=\"an.\"]');", 4))
    undo()
    check("and one undo brings the part and both configs' cables back", wait_for(
        "return !!document.querySelector('#bench-svg [data-part=\"an\"]') && !!document.querySelector('#bench-svg .bn-cable[data-ends=\"an.in|dut.usb-c-1\"]');", 4))

    # --- stacking by gesture ---------------------------------------------------
    select_part("drp")
    target = center('#bench-svg [data-part="partner"] .bn-body')
    press_drag('#bench-svg [data-part="drp"] .bn-body', target, steps=10)
    ghost = wait_for("var t=document.querySelector('#bench-svg .bn-snap-text'); return t ? t.textContent : '';", 6)
    check("a shield dragged over a board with free headers shows where it would sit", "pd-bench" in (ghost or ""), ghost)
    release_at(target)
    stacked = wait_for("return document.querySelector('#bench-svg [data-part=\"partner\"] .bn-sub').textContent.indexOf(' + ') >= 0;", 6)
    check("released there, it is mounted on the board, which names what it carries", stacked,
          script("return document.querySelector('#bench-svg [data-part=\"partner\"] .bn-sub').textContent;"))
    check("and drawn sitting on it", sits_on("drp", "partner"))
    # A second shield is refused by the headers the first now holds.
    click("#bench-part-btn")
    click('#bench-bin [data-template="x-nucleo-snk1m1"]')
    wait_for("return !!document.querySelector('#bench-svg [data-part=\"x-nucleo-snk1m1\"]');", 4)
    click("#bench-bin-close")
    target = center('#bench-svg [data-part="partner"] .bn-body')
    press_drag('#bench-svg [data-part="x-nucleo-snk1m1"] .bn-body', target, steps=10)
    full = wait_for("var w=document.querySelector('#bench-svg [data-why-box]'); return w ? w.textContent : '';", 6)
    check("a second shield over the same board is refused in the server's words", "already carries" in (full or ""), full)
    release_at(target)
    check("and released there it stays unmounted, the first shield still in place", not sits_on("x-nucleo-snk1m1", "partner") and sits_on("drp", "partner"))
    select_part("x-nucleo-snk1m1")
    key(DEL)
    wait_for("return !document.querySelector('#bench-svg [data-part=\"x-nucleo-snk1m1\"]');", 4)
    select_part("drp")
    drag_to('#bench-svg [data-part="drp"] .bn-body', screen_of(420, 600), steps=10)
    check("dragged off, the shield is unmounted where it was dropped", wait_for(
        "return document.querySelector('#bench-svg [data-part=\"partner\"] .bn-sub').textContent.indexOf(' + ') < 0;", 4) and not sits_on("drp", "partner"))

    # --- the tray ---------------------------------------------------------------
    tray = wait_for("return Array.from(document.querySelectorAll('#bench-tray [data-tray]')).map(function(c){return c.getAttribute('data-tray');});", 12)
    check("the tray lists what Core enumerates that no part binds",
          sorted(tray or []) == sorted(["probe:" + LOOSE, "port:COM7", "port:COM8", "port:COM9", "port:COM10"]), tray)
    if SHOTS:
        theme("dark"); shot("edit-tray-dark.png")
        theme("light"); shot("edit-tray-light.png")
        theme("dark")
    click('#bench-tray [data-tray="probe:%s"]' % LOOSE)
    check("clicking a probe arms it", "is-armed" in script("return document.querySelector('#bench-tray [data-tray=\"probe:%s\"]').className;" % LOOSE))
    check("and every usb-host connector that carries a probe pulses, one that carries none does not",
          "bn-tray-ok" in (classes(conn("debug-probe", "usb")) or "") and
          "bn-tray-ok" not in (classes(conn("usb-uart-bridge", "usb")) or ""))
    click(conn("debug-probe", "usb"))
    check("clicking one writes the probe's serial into the bench", wait_for(
        "return !document.querySelector('#bench-tray [data-tray=\"probe:%s\"]');" % LOOSE, 4) and LOOSE in notice(), notice())
    # COM7, alone under its serial, dragged onto the bridge's VCOM: its serial.
    select_part("usb-uart-bridge")
    allowed = dnd('#bench-tray [data-tray="port:COM7"]', *center(conn("usb-uart-bridge", "usb")))
    check("a port dragged onto a usb-host connector carrying a serial port is accepted", allowed)
    check("and is written by its serial alone", wait_for("return !document.querySelector('#bench-tray [data-tray=\"port:COM7\"]');", 4) and
          "vcom is FT7ONLY" in notice(), notice())
    bx, by = script("var r=document.getElementById('bench-wrap').getBoundingClientRect(); return [r.left + 40, r.top + r.height - 120];")
    check("a drop over bare canvas is refused", not dnd('#bench-tray [data-tray="port:COM8"]', bx, by))
    # COM8 shares its serial with COM9, onto a USB carrying three port functions.
    select_part("dfu")
    allowed = dnd('#bench-tray [data-tray="port:COM8"]', *center(conn("dfu", "usb")))
    asked = wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    check("a port dropped on a connector with several port functions asks which", allowed and asked and "bootload-app" in (dialog_text() or ""), dialog_text())
    script("document.querySelector('#bench-dialog [data-choice=\"bootload-app\"]').click();")
    check("and is written by serial and interface, since a sibling shares its serial", wait_for(
        "return !document.querySelector('#bench-tray [data-tray=\"port:COM8\"]');", 4) and "DUAL55 · interface 0" in notice(), notice())
    allowed = dnd('#bench-tray [data-tray="port:COM10"]', *center(conn("dfu", "usb")))
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    script("document.querySelector('#bench-dialog [data-choice=\"vcom\"]').click();")
    check("a port with no serial is written by its vid and pid", wait_for(
        "return !document.querySelector('#bench-tray [data-tray=\"port:COM10\"]');", 4) and "vid 0x1a86 · pid 0x7523" in notice(), notice())

    # --- the inspector ------------------------------------------------------------
    select_part("dfu")
    script("var i=document.querySelector('#bench-inspector [data-ed=\"label\"]'); i.value='flash target'; i.dispatchEvent(new Event('change', {bubbles:true}));")
    check("a label typed in the inspector renames the part on the canvas", wait_for(
        "return document.querySelector('#bench-svg [data-part=\"dfu\"] .bn-name').textContent === 'flash target';", 4))
    script("var r=document.querySelector('#bench-inspector [data-ed=\"role\"]'); r.value='dut'; r.dispatchEvent(new Event('change', {bubbles:true}));")
    checks_done()
    two = script("return Array.from(document.querySelectorAll('#bench-checks-list [data-finding]')).map(function(l){return l.textContent;}).join('|');")
    check("a role on a second part is the server's role-twice error, in the Checks panel", "role-twice" in (two or ""), two)
    check("drawn red on both parts", wait_for(
        "return document.querySelector('#bench-svg [data-flag=\"dfu\"]').classList.contains('bn-flag-error') &&"
        " document.querySelector('#bench-svg [data-flag=\"dut\"]').classList.contains('bn-flag-error');", 4))
    check("and the Checks button counts it as an error", script("return document.getElementById('bench-checks-btn').getAttribute('data-worst');") == "error")
    click("#bench-checks-btn")
    if SHOTS:
        theme("dark"); shot("edit-checks-dark.png")
        theme("light"); shot("edit-checks-light.png")
        theme("dark")
    script("var l=Array.from(document.querySelectorAll('#bench-checks-list [data-finding]')).filter(function(x){return x.textContent.indexOf('shield-unmounted')>=0;})[0]; l.click();")
    check("clicking a finding selects what it names, and the panel closes", wait_for(
        "return document.querySelector('#bench-svg [data-part=\"drp\"]').classList.contains('bn-sel') &&"
        " document.getElementById('topo-panel-checks').hidden;", 4))
    select_part("dfu")
    script("var r=document.querySelector('#bench-inspector [data-ed=\"role\"]'); r.value=''; r.dispatchEvent(new Event('change', {bubbles:true}));")
    select_part("partner")
    before = script("return document.querySelector(arguments[0]).getAttribute('data-x');", [conn("partner", "stlink")])
    key("f")
    check("F flips the selected part", wait_for("return document.querySelector(arguments[0]).getAttribute('data-x') !== arguments[1];", 4,
                                                [conn("partner", "stlink"), before]))
    undo()
    select_part("dfu")
    script("document.querySelector('#bench-inspector [data-act=\"unbind\"][data-fn=\"vcom\"]').click();")
    check("an unbind ✕ drops that binding, and its port is back in the tray", wait_for(
        "return !!document.querySelector('#bench-tray [data-tray=\"port:COM10\"]');", 4))

    # --- save, and the 409 after a hand edit ---------------------------------------
    if SHOTS:
        theme("dark"); shot("edit-unsaved-dark.png")
        theme("light"); shot("edit-unsaved-light.png")
        theme("dark")
    check("before Save the file on disk is untouched", file("edit-bench") == tomllib.loads(EDIT_BENCH))
    click("#bench-save")
    wait_for("return document.getElementById('bench-dirty').hidden;", 6)
    doc = file("edit-bench")
    check("Save writes the file: the placed parts", all(part_of(doc, p) for p in ["debug-probe", "usb-uart-bridge"]) and
          part_of(doc, "dfu").get("label") == "flash target", [p["id"] for p in doc.get("parts", [])])
    check("the cable to this machine at the bench level, the SWD lead in config A",
          {"from": "debug-probe.usb", "to": "host"} in doc.get("cables", []) and
          ["debug-probe.swd", "dut.swd"] in next(c for c in doc["configs"] if c["name"] == "A")["cables"], doc.get("cables"))
    check("the tray's bindings in the three forms",
          part_of(doc, "debug-probe").get("usb") == {"usb": {"probe": LOOSE}} and
          part_of(doc, "usb-uart-bridge").get("usb") == {"usb": {"vcom": "FT7ONLY"}} and
          part_of(doc, "dfu").get("usb") == {"usb": {"bootload-app": {"serial": "DUAL55", "interface": 0}}},
          [part_of(doc, p).get("usb") for p in ["debug-probe", "usb-uart-bridge", "dfu"]])
    check("the shield where it was left, mounted on nothing", not part_of(doc, "drp").get("mounted_on") and part_of(doc, "drp").get("at"))
    check("and the bar is clean again, Save disabled", not dirty() and script("return document.getElementById('bench-save').disabled;"))
    check("Save keeps the undo history", not script("return document.getElementById('bench-undo').disabled;"))
    check("nothing in this phase called Core", CORE_WRITES == [], CORE_WRITES)
    # A hand edit on disk, then a second save.
    text = (TOPO / "edit-bench.toml").read_text()
    (TOPO / "edit-bench.toml").write_text(text.replace('label = "pd-bench"', 'label = "edited by hand"'))
    select_part("debug-probe")
    key(DEL)
    wait_for("return !document.querySelector('#bench-svg [data-part=\"debug-probe\"]');", 4)
    save()
    conflict = wait_for("return document.getElementById('bench-dialog').style.display === 'block' ? document.getElementById('bench-dialog').innerText : '';", 6)
    check("a save after the file changed on disk is refused 409, and says so", "changed since it was read" in (conflict or ""), conflict)
    check("never a silent overwrite: the hand edit is still on disk", "edited by hand" in (TOPO / "edit-bench.toml").read_text())
    script("document.getElementById('bench-dialog-ok').click();")
    check("Reload reads the file again, the unsaved delete dropped", wait_for(
        "return !!document.querySelector('#bench-svg [data-part=\"debug-probe\"]') &&"
        " document.querySelector('#bench-svg [data-part=\"partner\"] .bn-name').textContent === 'edited by hand';", 6) and not dirty())

    # --- unsaved changes before leaving ---------------------------------------------
    select_part("debug-probe")
    key("f")
    wait_for("return !document.getElementById('bench-dirty').hidden;", 4)
    pick_bench("other-bench")
    check("switching bench with unsaved changes asks first, in the page", wait_for(
        "return document.getElementById('bench-dialog').style.display === 'block';", 4) and "Unsaved changes" in (dialog_text() or ""))
    script("document.getElementById('bench-dialog-cancel').click();")
    check("Keep editing stays on the bench, edits and all", script("return document.getElementById('bench-pick').value;") == "edit-bench" and dirty())
    script("document.querySelector('.nav-item[data-tab=\"desk\"]').click();")
    check("leaving the tab with unsaved changes asks too", wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4))
    key(ESC)
    check("Esc keeps editing, on the Topology tab", not dialog_open() and script(
        "return document.querySelector('.tab-panel.active').getAttribute('data-tab');") == "topology")
    check("a reload with unsaved changes has the browser ask", script(
        "var e=new Event('beforeunload', {cancelable:true}); window.dispatchEvent(e); return e.defaultPrevented;"))
    # Opening another repo by hand asks too, and Keep editing keeps the project.
    other_repo = Path(iso) / "fw-two"
    (other_repo / ".git").mkdir(parents=True)
    script("document.getElementById('project-button').click();")
    script("document.getElementById('project-path').value=arguments[0]; document.getElementById('project-open').click();", [str(other_repo)])
    asked = wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    script("document.getElementById('bench-dialog-cancel').click();")
    time.sleep(0.6)
    with urllib.request.urlopen("http://127.0.0.1:%s/api/study-designer/project" % UI_PORT, timeout=10) as r:
        still = json.loads(r.read().decode()).get("path")
    check("switching project with unsaved changes asks first, and Keep editing keeps the repo open", asked and still == str(fw) and dirty(), still)
    script("var d=document.getElementById('project-dialog'); if (d && d.style.display==='block') document.getElementById('project-cancel').click();")
    pick_bench("other-bench")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    script("document.getElementById('bench-dialog-ok').click();")
    check("Discard changes opens the other bench", on_bench("other-bench") and not dirty())
    check("and the discarded edit never reached the file", not part_of(file("edit-bench"), "debug-probe").get("flip"))

    # --- a new bench ------------------------------------------------------------------
    pick_bench("+new")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    script("document.getElementById('bench-dialog-input').value='Fresh Bench'; document.getElementById('bench-dialog-ok').click();")
    check("New bench… creates the file through POST and opens it", on_bench("fresh-bench"))
    fresh = file("fresh-bench") if (TOPO / "fresh-bench.toml").exists() else {}
    check("holding this machine as its only part and one config, A",
          [p["template"] for p in fresh.get("parts", [])] == ["this-machine"] and [c["name"] for c in fresh.get("configs", [])] == ["A"], fresh)
    pick_bench("+new")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    script("document.getElementById('bench-dialog-input').value='fresh bench'; document.getElementById('bench-dialog-ok').click();")
    taken = wait_for("var e=document.getElementById('bench-dialog-error'); return !e.hidden ? e.textContent : '';", 4)
    check("a name whose file exists is refused in the dialog, which stays open", "already exists" in (taken or ""), taken)
    script("document.getElementById('bench-dialog-cancel').click();")

    # --- the automatic config A -------------------------------------------------------
    pick_bench("loose-bench")
    on_bench("loose-bench")
    check("a bench with no config shows no config tabs", script("return document.getElementById('bench-config-row').hidden;"))
    select_part("dut")
    click(conn("dut", "usb-c-1"))
    wait_for("var e=document.querySelector(arguments[0]); return !!e && e.classList.contains('bn-ok');", 6, [conn("an", "in")])
    click(conn("an", "in"))
    check("its first part-to-part cable creates config A, and says so", wait_for(
        "return !document.getElementById('bench-config-row').hidden && document.querySelector('#bench-configs [data-cfg=\"0\"]').textContent === 'A';", 4)
        and "new config, A" in notice(), notice())
    save()
    wait_for("return document.getElementById('bench-dirty').hidden;", 6)
    check("and Save writes it", file("loose-bench").get("configs") == [{"name": "A", "cables": [["dut.usb-c-1", "an.in"]]}], file("loose-bench").get("configs"))
    save()
    check("Ctrl+S with nothing unsaved writes nothing and says so", "Nothing to save" in notice(), notice())

    check("no uncaught JS error or unhandled rejection, load included", script("return window.__errs;") == [], script("return window.__errs;"))
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
    shutil.rmtree(iso, ignore_errors=True)

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
