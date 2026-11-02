"""The part editor (`assets/bench-parts.js`), in a real browser, against the real binary.

Phase 3 of embarch-bench-editor-proposal.md: one repo part template edited in a
dialog over the Topology tab and written through `POST`/`PUT /api/bench/parts`.
Unlike drive_bench.py there is no fixture proxy: the routes are the binary's
own, over a scratch firmware repo, so every file this checks is one the server
really wrote and every refusal is the server's own text, compared with what the
same request answers outside the browser.

The canvas's parts bin opens the editor with `embarch:edit-part`; this drive
dispatches that event itself. The live preview is the canvas's
`window.embarchBench.previewTemplate`, which must draw and follow an edit; with
the canvas's object taken away, the dialog must say the canvas did not load and
draw nothing. The browser opens the page through a pass-through proxy that puts
an error listener at the top of it, so an error while `bench-parts.js` loads is
caught too.

Ports: geckodriver at EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, a desk and
a Core nothing listens on at +1 and +2 (no bench route calls either), and the
proxy the browser opens at +3. EMBARCH_UI_BIN picks the binary (default the
release build); EMBARCH_PARTS_SHOTS=<dir> saves screenshots of the dialog in
both themes.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_bench_parts.py
"""
import base64, json, os, shutil, subprocess, sys, tempfile, threading, time, tomllib, urllib.error, urllib.request
from http.client import HTTPConnection, HTTPException   # not `import http`: http() below is the UI helper
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

sys.dont_write_bytecode = True   # no __pycache__ left in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))
import addr  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
UI_PORT = addr.ui_port(4905)
UI = "http://127.0.0.1:" + UI_PORT
BASE = addr.gecko_url()
DESK_PORT = int(UI_PORT) + 1
CORE_PORT = int(UI_PORT) + 2
PROXY_PORT = int(UI_PORT) + 3
PAGE = "http://127.0.0.1:%d" % PROXY_PORT
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "release" / "embarch-ui"))
SHOTS = os.environ.get("EMBARCH_PARTS_SHOTS")
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"

fails = []


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


def http_full(method, path, body=None):
    """The UI's own answer, outside the browser: (status, text, the refusal's word)."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(UI + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode(), r.headers.get("x-embarch-refusal")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(), e.headers.get("x-embarch-refusal")


def http(method, path, body=None):
    return http_full(method, path, body)[:2]


def version_of(data):
    """The server's file version (mod.rs `version_of`): FNV-1a 64 of the bytes, and their count."""
    h = 0xcbf29ce484222325
    for byte in data:
        h = ((h ^ byte) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return "%016x-%d" % (h, len(data))


# The page's first script: an error while the page loads is an error too.
LISTEN = (b"<script>window.__errs=[];addEventListener('error',function(e){__errs.push(String(e.message))});"
          b"addEventListener('unhandledrejection',function(e){__errs.push('unhandled: '+String(e.reason&&e.reason.message||e.reason))});</script>")


class Proxy(BaseHTTPRequestHandler):
    """Everything to the UI; the page gets the error listener first."""
    protocol_version = "HTTP/1.0"   # a body ends with the connection, so the SSE stream passes as it comes

    def log_message(self, *a):
        pass

    def _forward(self):
        path = urlparse(self.path).path
        length = int(self.headers.get("content-length") or 0)
        data = self.rfile.read(length) if length else None
        drop = {"connection", "keep-alive", "accept-encoding", "content-length"} | ({"if-none-match"} if path == "/" else set())
        headers = {k: v for k, v in self.headers.items() if k.lower() not in drop}
        up = HTTPConnection("127.0.0.1", int(UI_PORT), timeout=120)
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
        for k, v in resp.getheaders():
            if k.lower() not in ("transfer-encoding", "connection", "content-length", "keep-alive", "etag"):
                self.send_header(k, v)
        if (resp.getheader("content-type") or "").startswith("text/event-stream"):
            self.end_headers()
            try:
                while True:
                    chunk = resp.read1(65536)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
            except (OSError, HTTPException):
                pass   # the UI or the browser closed the stream
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


def wd(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]


iso = tempfile.mkdtemp(prefix="embarch-ui-parts-drive-")
fw = Path(iso) / "fw"
(fw / ".git").mkdir(parents=True)
(fw / "embarch").mkdir()
parts_dir = fw / "embarch" / "parts"
cfg = Path(iso) / "ui.toml"
cfg.write_text('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "drive-token"\n\n'
               '[study_designer]\nfirmware_repo_path = "%s"\n' % (CORE_PORT, fw))
env = dict(os.environ, EMBARCH_UI_CONFIG=str(cfg), EMBARCH_UI_PORT=UI_PORT, EMBARCH_DESK_PORT=str(DESK_PORT),
           EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data", XDG_CONFIG_HOME=iso + "/config",
           EMBARCH_UI_STATE=iso + "/recent-projects.json", HOME=iso + "/home")
ui = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
proxy = ThreadingHTTPServer(("127.0.0.1", PROXY_PORT), Proxy)
proxy.daemon_threads = True
threading.Thread(target=proxy.serve_forever, daemon=True).start()
sid = None
try:
    end = time.time() + 30
    while time.time() < end:
        try:
            if http("GET", "/")[0] == 200:
                break
        except OSError:
            pass
        time.sleep(0.2)
    status, text = http("GET", "/api/bench/vocabulary")
    check("the scratch project is open: the vocabulary answers", status == 200, (status, text[:120]))
    VOCAB = json.loads(text)
    for key in ("kinds", "sides", "power_kinds", "interface_functions", "line_functions", "categories", "looks", "functions"):
        check("the vocabulary serves %s" % key, isinstance(VOCAB.get(key), list) and VOCAB[key], VOCAB.get(key))

    sid = wd("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
        "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
    S = BASE + "/session/" + sid
    wd("POST", S + "/window/rect", {"width": 1440, "height": 1000})

    def script(js, args=None):
        return wd("POST", S + "/execute/sync", {"script": js, "args": args or []})

    def wait_for(js, timeout=10.0, args=None):
        end = time.time() + timeout
        while time.time() < end:
            v = script(js, args)
            if v:
                return v
            time.sleep(0.15)
        return script(js, args)

    def element(css):
        return wd("POST", S + "/element", {"using": "css selector", "value": css})[ELEMENT]

    def click(css):
        wd("POST", S + "/element/%s/click" % element(css), {})

    def type_into(css, text):
        e = element(css)
        wd("POST", S + "/element/%s/clear" % e, {})
        wd("POST", S + "/element/%s/value" % e, {"text": text})

    def pick(css, value):
        """A select set the way a choice sets it: its value, then `change`."""
        return script("var s = document.querySelector(arguments[0]); s.value = arguments[1];"
                      "s.dispatchEvent(new Event('change', {bubbles: true})); return s.value", [css, value])

    def key(value):
        wd("POST", S + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": [
            {"type": "keyDown", "value": value}, {"type": "keyUp", "value": value}]}]})
        wd("DELETE", S + "/actions")

    def shot(name, at="top"):
        """Both themes; `at` is where the dialog is scrolled: its top, or its foot (a refusal)."""
        if not SHOTS:
            return
        os.makedirs(SHOTS, exist_ok=True)
        script("var d = document.getElementById('part-dialog'); d.scrollTop = arguments[0] === 'top' ? 0 : d.scrollHeight; return true", [at])
        for theme in ("dark", "light"):
            script("document.documentElement.setAttribute('data-theme', arguments[0]); return true", [theme])
            time.sleep(0.25)
            png = base64.b64decode(wd("GET", S + "/screenshot"))
            Path(SHOTS, "parts-%s-%s.png" % (name, theme)).write_bytes(png)
        script("document.documentElement.setAttribute('data-theme', 'dark'); return true")

    def mode():
        return script("var d = document.getElementById('part-dialog'); return d.style.display === 'block' ? d.getAttribute('data-mode') : 'closed'")

    def open_editor(pid):
        script("document.dispatchEvent(new CustomEvent('embarch:edit-part', {detail: {id: arguments[0]}})); return true", [pid])
        return wait_for("var d = document.getElementById('part-dialog'); var m = d.getAttribute('data-mode');"
                        "return d.style.display === 'block' && m !== 'loading' && m")

    def settled():
        return wait_for("return !document.getElementById('part-dialog').hasAttribute('data-busy')")

    def result():
        return script("var r = document.getElementById('part-result'); return r.style.display === 'block' ? r.textContent : ''")

    def visible(css):
        return script("var e = document.querySelector(arguments[0]); return !!e && !e.hidden && e.getClientRects().length > 0", [css])

    def conn(i, f):
        return "#part-conns tr.pe-conn[data-row='%d'] [data-f='%s']" % (i, f)

    def conn_ids():
        return script("return Array.from(document.querySelectorAll('#part-conns tr.pe-conn [data-f=id]')).map(function (e) { return e.value; })")

    def options_of(css):
        return script("return Array.from(document.querySelector(arguments[0]).options).filter(function (o) { return !o.disabled; })"
                      ".map(function (o) { return o.value; })", [css])

    def served(pid):
        cat = json.loads(http("GET", "/api/bench/parts")[1])
        return next((p for p in cat["parts"] if p["id"] == pid), None), cat

    def changed():
        return script("return window.__changed.length")

    wd("POST", S + "/url", {"url": PAGE + "/#topology"})
    check("the page loads the part editor and builds its dialog",
          wait_for("return !!document.getElementById('part-dialog') && !!document.getElementById('part-saved')") is True)
    check("the error listener was on the page before it loaded, and heard nothing",
          script("return Array.isArray(window.__errs) ? window.__errs : 'no listener'") == [],
          script("return window.__errs"))
    script("window.__changed = []; document.addEventListener('embarch:parts-changed', function (e) { __changed.push(e.detail); });"
           "window.__sent = []; window.__delay = 0; var real = window.fetch; window.fetch = function (u, i) {"
           "  if (String(u).indexOf('/api/bench/parts') >= 0 && i && i.method && i.method !== 'GET') {"
           "    __sent.push({url: String(u), method: i.method, body: i.body ? JSON.parse(i.body) : null});"
           "    if (window.__version && i.body) { var b = JSON.parse(i.body); b.version = window.__version; window.__version = null;"
           "      i = Object.assign({}, i, {body: JSON.stringify(b)}); __sent[__sent.length - 1].body = b; }"
           "    var wait = window.__delay; window.__delay = 0;"
           "    if (wait) return new Promise(function (r) { setTimeout(r, wait); }).then(function () { return real.call(window, u, i); });"
           "  }"
           "  return real.call(window, u, i); }; return true")
    check("the canvas provides the preview", script("return !!(window.embarchBench && typeof window.embarchBench.previewTemplate === 'function')"))
    check("the part editor answers whether it holds an unsaved draft",
          script("return !!window.embarchParts && typeof window.embarchParts.unsaved === 'function' && window.embarchParts.unsaved() === false"))
    TPL = VOCAB["template"]

    def unsaved():
        return script("return window.embarchParts.unsaved()")

    def preview_state():
        return script("return [document.getElementById('part-dialog').getAttribute('data-preview'),"
                      " document.getElementById('part-preview-svg').childElementCount,"
                      " document.getElementById('part-preview-note').hidden]")

    # ---- a new template -------------------------------------------------------
    m = open_editor(None)
    check("a new part opens the editor", m == "new", (m, script("return document.getElementById('part-problem-text').textContent")))
    check("the new part's id is editable and the path follows it",
          script("return !document.getElementById('part-id').disabled") and
          script("return document.getElementById('part-path').textContent") == "embarch/parts/<id>.toml")
    check("category offers exactly the served categories", options_of("#part-category") == VOCAB["categories"])
    check("look offers exactly the served looks", options_of("#part-look") == VOCAB["looks"])
    ps = preview_state()
    check("the preview is the canvas's drawing", ps[0] == "drawn" and ps[1] > 0 and ps[2] is True, ps)
    check("an unchanged new part is no unsaved draft", unsaved() is False)
    # Without the canvas's object the preview says so and draws nothing.
    script("window.__bench = window.embarchBench; window.embarchBench = undefined; return true")
    type_into("#part-label", "x")
    ps = preview_state()
    check("with no canvas, the dialog says the canvas did not load and draws nothing",
          ps[0] == "none" and ps[1] == 0 and ps[2] is False
          and "canvas did not load" in script("return document.getElementById('part-preview-note').textContent"), ps)
    script("window.embarchBench = window.__bench; return true")
    check("a typed field makes the draft unsaved", unsaved() is True)
    type_into("#part-label", "")
    check("typed back to what it was, the draft is unchanged", unsaved() is False)
    check("the number fields are text, captioned with the served range",
          script("return ['part-width', 'part-min-height', 'part-rows-top'].map(function (i) { var e = document.getElementById(i);"
                 " return [e.type, e.getAttribute('inputmode'), e.title]; })")
          == [["text", "numeric", "a whole number, %d\u2013%d" % (TPL["numbers"][f]["min"], TPL["numbers"][f]["max"])]
              for f in ("width", "min_height", "rows_top")])

    type_into("#part-id", "usb-c-meter")
    check("the path follows the id as it is typed",
          script("return document.getElementById('part-path').textContent") == "embarch/parts/usb-c-meter.toml")
    type_into("#part-label", "USB-C power meter")
    type_into("#part-subtitle", "inline, SCPI over USB")
    pick("#part-category", "instrument")
    pick("#part-look", "instrument")
    type_into("#part-width", "210")
    rows = [("in", "USB-C in", "usb-c", "left", "pass", ""), ("out", "USB-C out", "usb-c", "right", "pass", ""),
            ("usb", "USB", "usb-host", "top", None, ""), ("aux", "Aux power", "terminal", "right", "", "2")]
    for i, (cid, label, kind, side, power, maxc) in enumerate(rows):
        click("#part-add-conn")
        check("a connector row is added (%s), its id focused" % cid,
              script("return document.querySelectorAll('#part-conns tr.pe-conn').length") == i + 1
              and script("return document.activeElement === document.querySelector(arguments[0])", [conn(i, "id")]))
        type_into(conn(i, "id"), cid)
        type_into(conn(i, "label"), label)
        pick(conn(i, "kind"), kind)
        pick(conn(i, "side"), side)
        if power:
            pick(conn(i, "power"), power)
        if maxc:
            type_into(conn(i, "max_cables"), maxc)
    check("kind offers exactly the served kinds", options_of(conn(0, "kind")) == VOCAB["kinds"])
    check("side offers exactly the served sides", options_of(conn(0, "side")) == VOCAB["sides"])
    served_powers = [w for k in VOCAB["power"] for w in VOCAB["power"][k]]
    check("a power connector's role offers none plus the served roles, grouped by class",
          options_of(conn(0, "power")) == [""] + served_powers
          and script("return Array.from(document.querySelectorAll(arguments[0] + ' optgroup')).map(function (g) { return g.label; })", [conn(0, "power")])
          == list(VOCAB["power"].keys()))
    check("a kind that carries no power takes no power role", script("return !document.querySelector(arguments[0])", [conn(2, "power")])
          and VOCAB["hub_kind"] not in VOCAB["power_kinds"])
    check("only the hub's kind carries functions: one functions row, under usb",
          script("return Array.from(document.querySelectorAll('#part-conns tr.pe-fn-row')).map(function (r) { return r.getAttribute('data-row'); })") == ["2"])

    # usb carries a vcom on interface 2 and an SCPI line with its facts.
    click("#part-conns tr.pe-fn-row[data-row='2'] [data-act='fn-add']")
    fn0 = "#part-conns tr.pe-fn-row[data-row='2'] [data-fn='0'] "
    fn1 = "#part-conns tr.pe-fn-row[data-row='2'] [data-fn='1'] "
    check("a function offers exactly the served functions", options_of(fn0 + "[data-f=function]") == VOCAB["functions"])
    pick(fn0 + "[data-f=function]", "vcom")
    check("vcom takes an interface and no line facts",
          ("vcom" in VOCAB["interface_functions"]) and visible(fn0 + "[data-f=interface]")
          and not script("return !!document.querySelector(arguments[0])", [fn0 + "[data-f=baud]"]))
    type_into(fn0 + "[data-f=interface]", "2")
    click("#part-conns tr.pe-fn-row[data-row='2'] [data-act='fn-add']")
    pick(fn1 + "[data-f=function]", "scpi")
    facts = script("return Array.from(document.querySelectorAll(arguments[0] + '.pe-fact:not(.pe-fact-fn) > span')).map(function (e) { return e.textContent; })",
                   ["#part-conns tr.pe-fn-row[data-row='2'] [data-fn='1'] "])
    check("scpi takes the served line facts, in order, under their names, and no interface",
          facts == [lf["name"] for lf in TPL["line_facts"]]
          and not script("return !!document.querySelector(arguments[0])", [fn1 + "[data-f=interface]"]), facts)
    check("a numeric line fact is text captioned with its served range, a text one is not",
          script("return [document.querySelector(arguments[0]).type, document.querySelector(arguments[0]).title, document.querySelector(arguments[1]).getAttribute('inputmode')]",
                 [fn1 + "[data-f=baud]", fn1 + "[data-f=ack_ok]"])
          == ["text", "a whole number, %d\u2013%d" % (TPL["numbers"]["baud"]["min"], TPL["numbers"]["baud"]["max"]), None])
    check("the interface and max cables carry their served ranges",
          script("return [document.querySelector(arguments[0]).title, document.querySelector(arguments[1]).title]",
                 [fn0 + "[data-f=interface]", conn(2, "max_cables")])
          == ["a whole number, %d\u2013%d" % (TPL["numbers"]["interface"]["min"], TPL["numbers"]["interface"]["max"]),
              "a whole number, %d\u2013%d" % (TPL["numbers"]["max_cables"]["min"], TPL["numbers"]["max_cables"]["max"])])
    type_into(fn1 + "[data-f=baud]", "115200")
    type_into(fn1 + "[data-f=write_terminator]", "\\n")
    type_into(fn1 + "[data-f=read_terminator]", "\\r\\n")
    type_into(fn1 + "[data-f=min_interval_ms]", "50")
    type_into(fn1 + "[data-f=ack_ok]", "OK")
    type_into(fn1 + "[data-f=ack_err]", "ERR")

    # Passthrough: only the connectors whose kind makes up a bus are offered.
    click("#part-add-pass")
    keys = script("return Array.from(document.querySelectorAll('#part-pass [data-pass=\"0\"] [data-end=\"0\"] option')).filter(function (o) { return !o.disabled; })"
                  ".map(function (o) { return o.textContent; })")
    check("a passthrough end offers the power connectors only", keys == ["in", "out", "aux"], keys)
    script("var s = document.querySelectorAll('#part-pass [data-pass=\"0\"] select');"
           "function by(sel, t) { return Array.from(sel.options).filter(function (o) { return o.textContent === t; })[0].value; }"
           "s[0].value = by(s[0], 'in'); s[0].dispatchEvent(new Event('change', {bubbles: true}));"
           "s = document.querySelectorAll('#part-pass [data-pass=\"0\"] select');"
           "s[1].value = by(s[1], 'out'); s[1].dispatchEvent(new Event('change', {bubbles: true})); return true")
    # A renamed connector keeps its pair.
    type_into(conn(1, "id"), "out-c")
    pair = script("return Array.from(document.querySelectorAll('#part-pass [data-pass=\"0\"] select')).map(function (s) { return s.options[s.selectedIndex].textContent; })")
    check("renaming a connector keeps its passthrough pair", pair == ["in", "out-c"], pair)
    type_into(conn(1, "id"), "out")

    # Reorder and remove.
    click(conn(3, "id").replace("[data-f='id']", "[data-act='up']"))
    click(conn(2, "id").replace("[data-f='id']", "[data-act='up']"))
    check("moving a row up twice", conn_ids() == ["in", "aux", "out", "usb"], conn_ids())
    check("focus follows the moved row", script("return document.activeElement === document.querySelector(arguments[0])",
                                                [conn(1, "id").replace("[data-f='id']", "[data-act='up']")]))
    click(conn(1, "id").replace("[data-f='id']", "[data-act='down']"))
    click(conn(2, "id").replace("[data-f='id']", "[data-act='down']"))
    check("and back down twice", conn_ids() == ["in", "out", "usb", "aux"], conn_ids())
    check("the functions row moved with its connector",
          script("return document.querySelector('#part-conns tr.pe-fn-row').getAttribute('data-row')") == "2"
          and script("return document.querySelector(arguments[0]).value", [fn1 + "[data-f=baud]"]) == "115200")
    click("#part-add-conn")
    type_into(conn(4, "id"), "junk")
    click(conn(4, "id").replace("[data-f='id']", "[data-act='remove']"))
    check("a row is removed", conn_ids() == ["in", "out", "usb", "aux"], conn_ids())
    before = script("return document.getElementById('part-preview-svg').innerHTML")
    type_into("#part-label", "USB-C power meter, inline")
    check("the preview follows an edit", script("return document.getElementById('part-preview-svg').innerHTML") != before)
    type_into("#part-label", "USB-C power meter")
    # What a number field cannot hold is sent as typed, for the server to refuse.
    type_into("#part-width", "21O")
    click("#part-save")
    settled()
    status, text = http("POST", "/api/bench/parts", script("return window.__sent[window.__sent.length - 1].body"))
    check("a number field typed wrong reaches the server as typed, and is refused 400 naming the field",
          script("return window.__sent[window.__sent.length - 1].body.width") == "21O" and status == 400
          and text.startswith("not a part template: width: "), (status, text))
    check("the dialog shows that refusal verbatim, nothing is written", result() == text and not (parts_dir / "usb-c-meter.toml").exists(),
          (result(), text))
    type_into("#part-width", "210")
    type_into(fn0 + "[data-f=interface]", "two")
    click("#part-save")
    settled()
    check("a wrong number deep in the connectors is named by its path",
          result().startswith("not a part template: connectors[2].functions[0].interface: invalid type: string \"two\""), result())
    type_into(fn0 + "[data-f=interface]", "2")
    shot("new")
    n0 = changed()
    click("#part-save")
    check("Save closes the editor", wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True, result())
    check("embarch:parts-changed fires once, naming the part and its file", changed() == n0 + 1
          and script("return window.__changed[window.__changed.length - 1]") == {"id": "usb-c-meter", "file": "embarch/parts/usb-c-meter.toml"},
          script("return window.__changed"))
    check("the editor says where the file was written",
          visible("#part-saved") and script("return document.getElementById('part-saved-text').textContent") == "Created embarch/parts/usb-c-meter.toml")
    sent = script("return window.__sent[window.__sent.length - 1]")
    check("a new part is a POST to /api/bench/parts with no version", sent and sent["method"] == "POST"
          and sent["url"].endswith("/api/bench/parts") and "version" not in sent["body"], sent)
    path = parts_dir / "usb-c-meter.toml"
    check("its file appears", path.is_file())
    written = tomllib.loads(path.read_text()) if path.is_file() else {}
    want = {
        "id": "usb-c-meter", "label": "USB-C power meter", "subtitle": "inline, SCPI over USB",
        "category": "instrument", "look": "instrument", "width": 210,
        "passthrough": [["in", "out"]],
        "connectors": [
            {"id": "in", "label": "USB-C in", "kind": "usb-c", "side": "left", "power": "pass", "max_cables": 1},
            {"id": "out", "label": "USB-C out", "kind": "usb-c", "side": "right", "power": "pass", "max_cables": 1},
            {"id": "usb", "label": "USB", "kind": "usb-host", "side": "top", "max_cables": 1, "functions": [
                {"function": "vcom", "interface": 2},
                {"function": "scpi", "baud": 115200, "write_terminator": "\n", "read_terminator": "\r\n",
                 "min_interval_ms": 50, "ack_ok": "OK", "ack_err": "ERR"}]},
            {"id": "aux", "label": "Aux power", "kind": "terminal", "side": "right", "max_cables": 2},
        ],
    }
    check("the file is the template as edited, terminators unescaped, nothing hidden written", written == want, written)
    script("document.getElementById('part-saved-close').click(); return true")

    # ---- editing it: PUT with the version it read --------------------------------
    part, _ = served("usb-c-meter")
    v1 = part and part["version"]
    check("editing the repo part opens it", open_editor("usb-c-meter") == "edit")
    check("its id is fixed once created", script("return document.getElementById('part-id').disabled") is True)
    check("its fields are the file's",
          script("return document.getElementById('part-label').value") == "USB-C power meter"
          and conn_ids() == ["in", "out", "usb", "aux"]
          and script("return document.querySelector(arguments[0]).value", [fn1 + "[data-f=read_terminator]"]) == "\\r\\n")
    type_into("#part-label", "USB-C power meter, 65 W")
    shot("edit")
    click("#part-save")
    check("Save closes the editor", wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True, result())
    sent = script("return window.__sent[window.__sent.length - 1]")
    check("an edit is a PUT to /api/bench/parts/<id> naming the version it read",
          sent and sent["method"] == "PUT" and sent["url"].endswith("/api/bench/parts/usb-c-meter") and sent["body"].get("version") == v1, sent)
    check("the file carries the edit", tomllib.loads(path.read_text()).get("label") == "USB-C power meter, 65 W")
    check("and parts-changed fired again", changed() == n0 + 2)
    check("the saved line says Saved", script("return document.getElementById('part-saved-text').textContent") == "Saved embarch/parts/usb-c-meter.toml")

    # ---- a hand edit on disk, then Save: 409, and Reload --------------------------
    check("editing it again", open_editor("usb-c-meter") == "edit")
    type_into("#part-subtitle", "typed in the dialog")
    path.write_text(path.read_text().replace('label = "USB-C power meter, 65 W"', 'label = "Edited by hand"'))
    on_disk = path.read_text()
    click("#part-save")
    settled()
    part, _ = served("usb-c-meter")
    stale = dict(script("return window.__sent[window.__sent.length - 1].body"))
    status, text, word = http_full("PUT", "/api/bench/parts/usb-c-meter", stale)
    check("the server refuses the stale version 409, as version-changed", status == 409 and word == "version-changed", (status, text, word))
    check("the dialog shows the server's refusal verbatim", result() == text, (result(), text))
    check("the dialog stays open on its edit, offering Reload", mode() == "edit" and visible("#part-reload"))
    check("the file is the hand edit, untouched", path.read_text() == on_disk)
    shot("refused", "foot")
    click("#part-reload")
    wait_for("return document.getElementById('part-dialog').getAttribute('data-mode') === 'edit' && document.getElementById('part-label').value === 'Edited by hand'")
    check("Reload reads the file as it is now", script("return document.getElementById('part-label').value") == "Edited by hand"
          and script("return document.getElementById('part-subtitle').value") == "inline, SCPI over USB"
          and not visible("#part-reload") and result() == "")
    type_into("#part-subtitle", "after a reload")
    click("#part-save")
    check("a save after Reload goes through", wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True, result())
    sent = script("return window.__sent[window.__sent.length - 1]")
    check("naming the version read on Reload", sent["body"].get("version") == part["version"], (sent["body"].get("version"), part["version"]))
    after = tomllib.loads(path.read_text())
    check("the file keeps the hand edit and adds the new one", after.get("label") == "Edited by hand" and after.get("subtitle") == "after a reload", after)
    check("the drive's version is the server's", served("usb-c-meter")[0]["version"] == version_of(path.read_bytes()))

    # A file that no longer loads is refused as unparseable, and Reload is no way on.
    check("editing it once more", open_editor("usb-c-meter") == "edit")
    type_into("#part-subtitle", "over a broken file")
    good = path.read_bytes()
    broken = good + b"\n[[[\n"
    path.write_bytes(broken)
    script("window.__version = arguments[0]; return true", [version_of(broken)])   # as if it had read the broken file
    click("#part-save")
    settled()
    status, text, word = http_full("PUT", "/api/bench/parts/usb-c-meter", script("return window.__sent[window.__sent.length - 1].body"))
    check("the server refuses a file that does not load 409, as unparseable", status == 409 and word == "unparseable", (status, text, word))
    check("the dialog shows it verbatim and offers no Reload", result() == text and not visible("#part-reload") and mode() == "edit",
          (result(), text))
    check("the broken file is left alone", path.read_bytes() == broken)
    path.write_bytes(good)
    key("\uE00C")
    click("#part-discard-yes")

    # ---- a built-in id is refused with the server's text ---------------------------
    check("a new part again", open_editor(None) == "new")
    type_into("#part-id", "cy4500")
    type_into("#part-label", "Mine")
    pick("#part-category", "analyzer")
    pick("#part-look", "analyzer")
    click("#part-save")
    settled()
    status, text, word = http_full("POST", "/api/bench/parts", script("return window.__sent[window.__sent.length - 1].body"))
    check("the server refuses a built-in's id 409, as builtin-id", status == 409 and "built-in" in text and word == "builtin-id", (status, text, word))
    check("the dialog shows that refusal verbatim", result() == text, (result(), text))
    check("nothing is written under the built-in's id", not (parts_dir / "cy4500.toml").exists())
    check("the dialog stays open as a new part", mode() == "new")

    # ---- an unknown kind cannot be saved -------------------------------------------
    type_into("#part-id", "odd-kind")
    click("#part-add-conn")
    type_into(conn(0, "id"), "x")
    type_into(conn(0, "label"), "X")
    check("the kind select holds only the served kinds", options_of(conn(0, "kind")) == VOCAB["kinds"])
    check("a kind outside them cannot be picked", pick(conn(0, "kind"), "usb-d") != "usb-d")
    # Forced past the select, as a newer page or a hand-edited request might:
    # the server refuses, and the dialog says so in its words.
    script("var s = document.querySelector(arguments[0]); var o = document.createElement('option'); o.value = o.textContent = 'usb-d';"
           "s.appendChild(o); s.value = 'usb-d'; s.dispatchEvent(new Event('change', {bubbles: true})); return true", [conn(0, "kind")])
    click("#part-save")
    settled()
    body = script("return window.__sent[window.__sent.length - 1].body")
    status, text = http("POST", "/api/bench/parts", body)
    check("the server refuses an unknown kind 400", body["connectors"][0]["kind"] == "usb-d" and status == 400 and "usb-d" in text, (status, text))
    check("the dialog shows that refusal verbatim", result() == text, (result(), text))
    check("and nothing is written", not (parts_dir / "odd-kind.toml").exists())
    click("#part-cancel")
    check("Cancel on that changed draft asks first", mode() == "new" and visible("#part-discard"))
    click("#part-discard-yes")
    check("and Discard closes it", mode() == "closed")

    # ---- a built-in: read-only, and Copy to this repo ------------------------------
    check("a built-in opens read-only", open_editor("scpi-load") == "builtin")
    check("every field is read-only and nothing can be added, moved or removed",
          script("return Array.from(document.querySelectorAll('#part-form input, #part-form select')).every(function (e) { return e.disabled; })")
          and script("return Array.from(document.querySelectorAll('#part-dialog [data-act]')).every(function (e) { return e.getClientRects().length === 0; })"))
    check("it offers Copy to this repo and no Save", visible("#part-copy") and not visible("#part-save"))
    shot("builtin")
    click("#part-copy")
    check("Copy opens a new part under another id",
          mode() == "new" and script("return document.getElementById('part-id').value") == "scpi-load-copy"
          and not script("return document.getElementById('part-id').disabled"))
    check("with the built-in's connectors and line facts",
          conn_ids() == ["in", "usb"] and script("return document.querySelector(\"#part-conns tr.pe-fn-row [data-f=write_terminator]\").value") == "\\n")
    click("#part-save")
    check("the copy saves", wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True, result())
    copy = tomllib.loads((parts_dir / "scpi-load-copy.toml").read_text()) if (parts_dir / "scpi-load-copy.toml").is_file() else {}
    builtin, _ = served("scpi-load")
    check("its file is the built-in under the new id",
          copy.get("id") == "scpi-load-copy" and copy.get("label") == builtin["label"]
          and copy.get("connectors", [{}, {}])[1].get("functions") == builtin["connectors"][1]["functions"], copy)

    # ---- a template the server cannot read ------------------------------------------
    (parts_dir / "broken.toml").write_text('id = "broken"\nlabel = "Broken"\ncategory = "board"\nlook = "board"\n'
                                           '[[connectors]]\nid = "a"\nlabel = "A"\nkind = "usb-d"\nside = "left"\n')
    _, cat = served("broken")
    err = next((e for e in cat["errors"] if e.get("id") == "broken"), None)
    check("the server lists it in errors with its id", err is not None, cat["errors"])
    check("opening it shows the error", open_editor("broken") == "error")
    check("with the server's words for the problem, verbatim, and the file",
          script("return document.getElementById('part-problem-text').textContent") == err["error"]
          and "embarch/parts/broken.toml" in script("return document.getElementById('part-problem-note').textContent"))
    check("no form, no Save; a fresh start is offered", not visible("#part-form") and not visible("#part-save") and visible("#part-fresh"))
    shot("unreadable")
    click("#part-fresh")
    check("a fresh start asks first, saying the file must go by hand",
          visible("#part-confirm") and "removed or fixed by hand" in script("return document.getElementById('part-confirm-text').textContent")
          and mode() == "error")
    click("#part-confirm-no")
    check("Back leaves the error as it was", not visible("#part-confirm") and visible("#part-fresh"))
    click("#part-fresh")
    click("#part-confirm-yes")
    check("confirmed, a new part under that id, fixed",
          mode() == "new" and script("return document.getElementById('part-id').value") == "broken"
          and script("return document.getElementById('part-id').disabled") is True and visible("#part-problem"))
    type_into("#part-label", "Broken, rewritten")
    pick("#part-category", "board")
    pick("#part-look", "board")
    click("#part-save")
    settled()
    status, text, word = http_full("POST", "/api/bench/parts", script("return window.__sent[window.__sent.length - 1].body"))
    check("the server will not create over the unreadable file, as exists", status == 409 and word == "exists", (status, text, word))
    check("the dialog shows that verbatim and says the file must go by hand",
          result() == text and "remove it or fix it by hand" in script("return document.getElementById('part-hint').textContent"))
    check("the unreadable file is untouched", "usb-d" in (parts_dir / "broken.toml").read_text())
    (parts_dir / "broken.toml").unlink()
    click("#part-save")
    check("once it is removed by hand, Save creates the fresh one",
          wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True
          and tomllib.loads((parts_dir / "broken.toml").read_text()).get("label") == "Broken, rewritten", result())

    # A file under a built-in's id: the built-in opens, and says the file is unused;
    # opened as an error there is no fresh start, since a repo may never take that id.
    (parts_dir / "nrf54l15dk.toml").write_text('id = "nrf54l15dk"\nlabel = "Mine"\ncategory = "board"\nlook = "devkit"\n')
    check("a built-in whose id a repo file reuses opens as the built-in", open_editor("nrf54l15dk") == "builtin")
    check("and says the repo file is not used", visible("#part-problem")
          and "not used" in script("return document.getElementById('part-problem-note').textContent"))
    click("#part-cancel")

    # ---- Esc: a Topology dialog, closed before a panel, never under Core's prompt ---
    check("Esc closes the editor", open_editor("usb-c-meter") == "edit")
    key("")
    check("…closed", wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True)
    click('.topo-bar [data-topo-panel="saved"]')
    check("a panel is open", visible("#topo-panel-saved"))
    open_editor("usb-c-meter")
    key("")
    check("Esc over a panel closes the editor first, leaving the panel",
          mode() == "closed" and visible("#topo-panel-saved"))
    key("")
    check("and the next Esc closes the panel", not visible("#topo-panel-saved"))
    open_editor("usb-c-meter")
    script("document.getElementById('prompt-dialog').style.display = 'block'; return true")
    key("")
    check("with Core's prompt over it, Esc leaves the editor alone", mode() == "edit")
    script("document.getElementById('prompt-dialog').style.display = 'none'; return true")
    key("")
    check("and closes it once the prompt is gone", mode() == "closed")

    # ---- a changed draft is never dropped silently -------------------------------------
    check("an unchanged part is no unsaved draft", open_editor("usb-c-meter") == "edit" and unsaved() is False)
    original = script("return document.getElementById('part-subtitle').value")
    type_into("#part-subtitle", "changed, not saved")
    check("a changed field makes it an unsaved draft", unsaved() is True)
    key("\uE00C")
    check("Esc on a changed draft asks first, and the dialog stays",
          mode() == "edit" and visible("#part-discard") and "not saved" in script("return document.getElementById('part-discard-text').textContent"))
    check("focus is on Keep editing", script("return document.activeElement && document.activeElement.id") == "part-discard-no")
    shot("discard", "foot")
    key("\uE00C")
    check("Esc again keeps editing: the question goes, the draft stays, focus is back in the field",
          mode() == "edit" and not visible("#part-discard") and unsaved() is True
          and script("return document.getElementById('part-subtitle').value") == "changed, not saved"
          and script("return document.activeElement && document.activeElement.id") == "part-subtitle")
    click("#part-cancel")
    check("Cancel on a changed draft asks too", mode() == "edit" and visible("#part-discard"))
    click("#part-discard-yes")
    check("Discard closes it, and nothing is unsaved", mode() == "closed" and unsaved() is False)
    check("the file is untouched by a discarded draft", tomllib.loads(path.read_text()).get("subtitle") == original)
    open_editor("usb-c-meter")
    type_into("#part-subtitle", "changed")
    type_into("#part-subtitle", original)
    key("\uE00C")
    check("a draft typed back to the file closes on Esc without asking", mode() == "closed")

    click('.topo-bar [data-topo-panel="saved"]')
    open_editor("usb-c-meter")
    type_into("#part-subtitle", "changed over a panel")
    key("\uE00C")
    check("over a panel, Esc on a changed draft asks and keeps focus on the question, the panel staying",
          mode() == "edit" and visible("#part-discard") and visible("#topo-panel-saved")
          and script("return document.activeElement && document.activeElement.id") == "part-discard-no")
    click("#part-discard-yes")
    check("Discard closes the editor and leaves the panel", mode() == "closed" and visible("#topo-panel-saved"))
    key("\uE00C")

    open_editor("usb-c-meter")
    type_into("#part-subtitle", "changed, then another part opened")
    script("document.dispatchEvent(new CustomEvent('embarch:edit-part', {detail: {id: 'scpi-load'}})); return true")
    check("opening another part over a changed draft asks first, naming it",
          mode() == "edit" and visible("#part-discard") and "`scpi-load`" in script("return document.getElementById('part-discard-text').textContent"))
    click("#part-discard-yes")
    check("Discard opens the part asked for", wait_for("return document.getElementById('part-dialog').getAttribute('data-mode') === 'builtin'") is True)
    check("a built-in is never an unsaved draft", unsaved() is False)
    click("#part-cancel")

    open_editor("usb-c-meter")
    type_into("#part-subtitle", "changed, then the project switched")
    script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: '/elsewhere'}})); return true")
    check("a project switch keeps a changed draft on screen, unsaveable, saying why",
          mode() == "edit" and visible("#part-stale") and script("return document.getElementById('part-save').disabled") is True
          and "cannot be saved" in script("return document.getElementById('part-stale').textContent") and unsaved() is True)
    shot("stale")
    n_sent = script("return window.__sent.length")
    click("#part-save")
    key("\uE007")
    check("and neither Save nor Enter sends it", script("return window.__sent.length") == n_sent)
    key("\uE00C")
    click("#part-discard-yes")
    check("it closes once discarded", mode() == "closed")
    open_editor("usb-c-meter")
    script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: '/elsewhere'}})); return true")
    check("a project switch closes an unchanged one", mode() == "closed")
    open_editor("usb-c-meter")
    type_into("#part-subtitle", "changed before the page's first project read")
    script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: '/elsewhere', initial: true}})); return true")
    check("the page's first project read is no switch: the draft stays saveable",
          mode() == "edit" and not visible("#part-stale") and script("return document.getElementById('part-save').disabled") is False)
    key("\uE00C")
    click("#part-discard-yes")
    open_editor("usb-c-meter")
    script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: '/elsewhere', initial: true}})); return true")
    check("nor does it close an unchanged one", mode() == "edit")
    script("location.hash = '#debug'; return true")
    check("leaving the Topology tab takes an unchanged dialog with it",
          wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True
          and script("return !!document.querySelector('.tab-panel.active[data-tab=\"debug\"]')"))
    script("location.hash = '#topology'; return true")
    wait_for("return !!document.querySelector('.tab-panel.active[data-tab=\"topology\"]')")

    # ---- the connector cap is the server's ------------------------------------------
    open_editor(None)
    cap = TPL["max_connectors"]
    script("var b = document.getElementById('part-add-conn'); for (var i = 0; i < arguments[0] + 3; i++) b.click(); return true", [cap])
    check("Add stops at the served cap", script("return document.querySelectorAll('#part-conns tr.pe-conn').length") == cap)
    check("and is disabled, saying why",
          script("return document.getElementById('part-add-conn').disabled") is True and visible("#part-conns-full")
          and script("return document.getElementById('part-conns-full').textContent") == "A template holds at most %d connectors." % cap)
    click(conn(0, "id").replace("[data-f='id']", "[data-act='remove']"))
    check("one removed, Add is back", script("return document.getElementById('part-add-conn').disabled") is False and not visible("#part-conns-full"))
    key("\uE00C")
    click("#part-discard-yes")

    # A save still out when its draft is discarded is announced anyway: the file was written.
    open_editor("usb-c-meter")
    type_into("#part-subtitle", "saved while closing")
    n1 = changed()
    script("window.__delay = 800; return true")
    click("#part-save")
    key("\uE00C")
    check("Esc while the save is out asks, since the draft is not saved yet", mode() == "edit" and visible("#part-discard"))
    click("#part-discard-yes")
    check("discarded, the editor closes while its save is out", mode() == "closed")
    check("the save still lands and is announced",
          wait_for("return window.__changed.length === arguments[0]", 10, [n1 + 1]) is True
          and tomllib.loads(path.read_text()).get("subtitle") == "saved while closing"
          and script("return document.getElementById('part-saved-text').textContent") == "Saved embarch/parts/usb-c-meter.toml")

    # ---- a part file removed on disk since the editor read it ---------------------------
    check("editing the part again", open_editor("usb-c-meter") == "edit")
    type_into("#part-subtitle", "after the file went")
    path.unlink()
    click("#part-save")
    settled()
    status, text, word = http_full("PUT", "/api/bench/parts/usb-c-meter", script("return window.__sent[window.__sent.length - 1].body"))
    check("the server answers a removed file 404", status == 404, (status, text))
    check("the dialog shows that verbatim, says the file was removed on disk, and offers Save as new, not Reload",
          result() == text and visible("#part-save-new") and not visible("#part-reload")
          and "removed on disk" in script("return document.getElementById('part-hint').textContent"), (result(), text))
    n2 = changed()
    click("#part-save-new")
    check("Save as new creates the part from the draft and closes",
          wait_for("return document.getElementById('part-dialog').style.display === 'none'") is True, result())
    sent = script("return window.__sent[window.__sent.length - 1]")
    check("as a POST naming no version", sent["method"] == "POST" and sent["url"].endswith("/api/bench/parts") and "version" not in sent["body"], sent)
    check("its file is back with the draft in it, announced as created",
          path.is_file() and tomllib.loads(path.read_text()).get("subtitle") == "after the file went" and changed() == n2 + 1
          and script("return document.getElementById('part-saved-text').textContent") == "Created embarch/parts/usb-c-meter.toml")

    # ---- phone width ------------------------------------------------------------------
    wd("POST", S + "/window/rect", {"width": 400, "height": 860})
    open_editor("usb-c-meter")
    time.sleep(0.3)
    check("at phone width the page does not scroll sideways",
          script("return document.documentElement.scrollWidth <= window.innerWidth"),
          script("return [document.documentElement.scrollWidth, window.innerWidth]"))
    check("and the dialog fits the screen",
          script("var r = document.getElementById('part-dialog').getBoundingClientRect(); return r.left >= 0 && r.right <= window.innerWidth"))
    if SHOTS:
        Path(SHOTS).mkdir(parents=True, exist_ok=True)
        Path(SHOTS, "parts-phone-dark.png").write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))
    click("#part-cancel")

    errs = script("return window.__errs")
    check("no script error on the page", errs == [], errs)
finally:
    if sid:
        try:
            wd("DELETE", BASE + "/session/" + sid)
        except Exception:
            pass
    ui.terminate()
    try:
        ui.wait(10)
    except subprocess.TimeoutExpired:
        ui.kill()
    shutil.rmtree(iso, ignore_errors=True)

print("\n%d failed" % len(fails) if fails else "\nall passed")
sys.exit(1 if fails else 0)
