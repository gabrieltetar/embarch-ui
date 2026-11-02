"""The part editor (`assets/bench-parts.js`), in a real browser, against the real binary.

Phase 3 of embarch-bench-editor-proposal.md: one repo part template edited in a
dialog over the Topology tab and written through `POST`/`PUT /api/bench/parts`.
Unlike drive_bench.py there is no fixture proxy: the routes are the binary's
own, over a scratch firmware repo, so every file this checks is one the server
really wrote and every refusal is the server's own text, compared with what the
same request answers outside the browser.

The canvas's parts bin opens the editor with `embarch:edit-part`; this drive
dispatches that event itself, so it runs whether or not the bin is there. The
live preview is the canvas's `window.embarchBench.previewTemplate`: when the
page has it the drive checks a drawing appears and follows an edit, and when it
does not, that the dialog says the canvas did not load and draws nothing.

Ports: geckodriver at EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, and the two
ports after it for a desk and a Core nothing listens on (no bench route calls
either). EMBARCH_UI_BIN picks the binary (default the release build);
EMBARCH_PARTS_SHOTS=<dir> saves screenshots of the dialog in both themes.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_bench_parts.py
"""
import base64, json, os, shutil, subprocess, sys, tempfile, time, tomllib, urllib.error, urllib.request
from pathlib import Path

sys.dont_write_bytecode = True   # no __pycache__ left in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))
import addr  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
UI_PORT = addr.ui_port(4905)
UI = "http://127.0.0.1:" + UI_PORT
BASE = addr.gecko_url()
DESK_PORT = int(UI_PORT) + 1
CORE_PORT = int(UI_PORT) + 2
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "release" / "embarch-ui"))
SHOTS = os.environ.get("EMBARCH_PARTS_SHOTS")
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"

fails = []


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


def http(method, path, body=None):
    """The UI's own answer, outside the browser: (status, text)."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(UI + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


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

    wd("POST", S + "/url", {"url": UI + "/#topology"})
    check("the page loads the part editor and builds its dialog",
          wait_for("return !!document.getElementById('part-dialog') && !!document.getElementById('part-saved')") is True)
    script("window.__changed = []; document.addEventListener('embarch:parts-changed', function (e) { __changed.push(e.detail); });"
           "window.__errs = []; addEventListener('error', function (e) { __errs.push(String(e.message)); });"
           "window.__sent = []; window.__delay = 0; var real = window.fetch; window.fetch = function (u, i) {"
           "  if (String(u).indexOf('/api/bench/parts') >= 0 && i && i.method && i.method !== 'GET') {"
           "    __sent.push({url: String(u), method: i.method, body: i.body ? JSON.parse(i.body) : null});"
           "    var wait = window.__delay; window.__delay = 0;"
           "    if (wait) return new Promise(function (r) { setTimeout(r, wait); }).then(function () { return real.call(window, u, i); });"
           "  }"
           "  return real.call(window, u, i); }; return true")
    HAS_PREVIEW = script("return !!(window.embarchBench && typeof window.embarchBench.previewTemplate === 'function')")
    print("      (the canvas %s previewTemplate)" % ("provides" if HAS_PREVIEW else "does not provide"))

    # ---- a new template -------------------------------------------------------
    m = open_editor(None)
    check("a new part opens the editor", m == "new", (m, script("return document.getElementById('part-problem-text').textContent")))
    check("the new part's id is editable and the path follows it",
          script("return !document.getElementById('part-id').disabled") and
          script("return document.getElementById('part-path').textContent") == "embarch/parts/<id>.toml")
    check("category offers exactly the served categories", options_of("#part-category") == VOCAB["categories"])
    check("look offers exactly the served looks", options_of("#part-look") == VOCAB["looks"])
    if HAS_PREVIEW:
        check("the preview is the canvas's drawing", script("return document.getElementById('part-dialog').getAttribute('data-preview')") == "drawn"
              and script("return document.getElementById('part-preview-svg').childElementCount") > 0)
    else:
        check("no canvas preview: the dialog says the canvas did not load and draws nothing",
              visible("#part-preview-note")
              and "canvas did not load" in script("return document.getElementById('part-preview-note').textContent")
              and script("return document.getElementById('part-preview-svg').childElementCount") == 0)

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
    check("scpi takes the line facts under the template's own names, and no interface",
          all(visible(fn1 + "[data-f=%s]" % f) for f in ("baud", "write_terminator", "read_terminator", "min_interval_ms", "ack_ok", "ack_err"))
          and not script("return !!document.querySelector(arguments[0])", [fn1 + "[data-f=interface]"]))
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
    if HAS_PREVIEW:
        before = script("return document.getElementById('part-preview-svg').innerHTML")
        type_into("#part-label", "USB-C power meter, inline")
        check("the preview follows an edit", script("return document.getElementById('part-preview-svg').innerHTML") != before)
        type_into("#part-label", "USB-C power meter")
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
    status, text = http("PUT", "/api/bench/parts/usb-c-meter", stale)
    check("the server refuses the stale version 409", status == 409, (status, text))
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

    # ---- a built-in id is refused with the server's text ---------------------------
    check("a new part again", open_editor(None) == "new")
    type_into("#part-id", "cy4500")
    type_into("#part-label", "Mine")
    pick("#part-category", "analyzer")
    pick("#part-look", "analyzer")
    click("#part-save")
    settled()
    status, text = http("POST", "/api/bench/parts", script("return window.__sent[window.__sent.length - 1].body"))
    check("the server refuses a built-in's id 409", status == 409 and "built-in" in text, (status, text))
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
    check("Cancel closes it", mode() == "closed")

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
    status, text = http("POST", "/api/bench/parts", script("return window.__sent[window.__sent.length - 1].body"))
    check("the server will not create over the unreadable file", status == 409, (status, text))
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

    # A save still out when Esc closes the dialog is announced anyway: the file was written.
    open_editor("usb-c-meter")
    type_into("#part-subtitle", "saved while closing")
    n1 = changed()
    script("window.__delay = 800; return true")
    click("#part-save")
    key("\uE00C")
    check("Esc closes the editor while its save is out", mode() == "closed")
    check("the save still lands and is announced",
          wait_for("return window.__changed.length === arguments[0]", 10, [n1 + 1]) is True
          and tomllib.loads(path.read_text()).get("subtitle") == "saved while closing"
          and script("return document.getElementById('part-saved-text').textContent") == "Saved embarch/parts/usb-c-meter.toml")

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
