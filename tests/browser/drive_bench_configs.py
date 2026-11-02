"""Config slots on the bench canvas, in a real browser (the bench editor's phase 5).

A config is a named set of part-to-part cables. Adding one (empty or a copy of
the config on screen), renaming and deleting one are bench edits on the config
row — undoable, written by Save — and **switching configs never calls Core**
(owner, 2026-11-01): on the live bench, "Make <config> live" records the config
through `POST /api/bench/live/config` and sends Core nothing, and a save that
renamed the live config carries the record to the new name before it declares
the bench again. A name's shape is the server's: the dialog refuses with the
vocabulary's own sentence, and a save of a file holding a bad name shows the
server's refusal verbatim.

And the inspector's board type on a **bench board** (a role-less board-category
part): picked from the project's catalog, written as the part's `board`, so a
make-live proposes its enrolment with no role and its label as the name.

**Every route is the real binary's**, on a scratch firmware repo, over a stub
Core that records what it is sent. Ports: geckodriver at EMBARCH_GECKO_PORT;
the UI at EMBARCH_UI_PORT, the stub Core at +2 and a desk port nothing listens
on at +3. EMBARCH_BENCH_SHOTS=<dir> saves screenshots in both themes.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_bench_configs.py
"""
import base64, json, os, re, shutil, subprocess, sys, tempfile, threading, time, tomllib, urllib.request
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

TOKEN = "stub-token"
UI_PORT = addr.ui_port(4875)
BASE = addr.gecko_url()
CORE_PORT = int(UI_PORT) + 2
DESK_PORT = int(UI_PORT) + 3
PAGE = "http://127.0.0.1:%s" % UI_PORT
SHOTS = os.environ.get("EMBARCH_BENCH_SHOTS")
NOW_MS = 1758240000000

DUT_PROBE = "DUTPROBE01"
NUC_PROBE = "NUCPROBE02"
PROBES = [{"identifier": "J-Link", "vendor_id": 4966, "product_id": 257, "serial_number": DUT_PROBE},
          {"identifier": "ST-LINK", "vendor_id": 1155, "product_id": 14158, "serial_number": NUC_PROBE}]
ENROLLED = []
ENROLLS = []
CORE_WRITES = []

BENCH = """name = "cfg-bench"
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
at = [380, 60]
usb = { usb = { probe = "%s" } }

[[parts]]
id = "partner"
template = "nucleo-g0b1re"
label = "pd-bench"
at = [380, 400]
usb = { stlink = { probe = "%s" } }

[[parts]]
id = "an"
template = "cy4500"
label = "Analyzer"
at = [760, 60]

[[parts]]
id = "brk"
template = "usb-c-breakout"
at = [760, 400]

[[cables]]
from = "dut.usb"
to = "host"

[[cables]]
from = "partner.stlink"
to = "host"

[[configs]]
name = "A"
cables = [["an.out", "brk.usb-c"]]
""" % (DUT_PROBE, NUC_PROBE)

BARE_BENCH = """name = "bare-bench"
saved_at_utc_ms = 2
parts = [{ id = "host", template = "this-machine", at = [20, 110] }, { id = "spare", template = "generic-board", at = [380, 110] }]
"""

# A config name a host study could not write (edge whitespace), as a hand edit leaves it.
ODD_BENCH = """name = "odd-bench"
saved_at_utc_ms = 2
parts = [{ id = "host", template = "this-machine", at = [20, 110] }, { id = "spare", template = "generic-board", at = [380, 110] }]

[[configs]]
name = " A"
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
        global ENROLLED
        path = urlparse(self.path).path
        body = self._body()
        CORE_WRITES.append(("POST", path))
        if path == "/probes/enroll":
            ENROLLS.append(body)
            board = {"probe_serial": body.get("probe_serial") or "", "role": body["role"], "name": body.get("name") or "",
                     "chip": body["chip"], "hardware_id": "ee:ff", "confirmed_at_utc_ms": NOW_MS, "link_serial": None}
            ENROLLED = [b for b in ENROLLED if b["probe_serial"] != board["probe_serial"] and not (board["role"] and b["role"] == board["role"])] + [board]
            return self._send(200, board)
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


def serve(port, handler):
    srv = ThreadingHTTPServer(("127.0.0.1", port), handler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode() or "{}")


def ui_json(path):
    return rq("GET", PAGE + path)


fails = []


def check(label, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + label + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(label)


core = serve(CORE_PORT, Core)
iso = tempfile.mkdtemp(prefix="embarch-ui-bench-configs-")
fw = Path(iso) / "fw"
(fw / ".git").mkdir(parents=True)
TOPO = fw / "embarch" / "topologies"
TOPO.mkdir(parents=True)
(TOPO / "cfg-bench.toml").write_text(BENCH)
(TOPO / "bare-bench.toml").write_text(BARE_BENCH)
(TOPO / "odd-bench.toml").write_text(ODD_BENCH)
(fw / "embarch" / "boards.toml").write_text(
    '[[boards]]\nname = "nrf54l15dk/nrf54l15/cpuapp"\nchip = "nRF54L15"\nbuild_target = "nrf54l15dk/nrf54l15/cpuapp"\n\n'
    '[[boards]]\nname = "nucleo_g0b1re"\nchip = "STM32G0B1RE"\nbuild_target = "nucleo_g0b1re"\n')
cfg = Path(iso) / "ui.toml"
cfg.write_text('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n\n[study_designer]\nfirmware_repo_path = "%s"\n'
               % (CORE_PORT, TOKEN, fw))
env = dict(os.environ, EMBARCH_UI_CONFIG=str(cfg), EMBARCH_UI_PORT=UI_PORT, EMBARCH_DESK_PORT=str(DESK_PORT),
           EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data", XDG_CONFIG_HOME=iso + "/config",
           EMBARCH_UI_STATE=iso + "/recent-projects.json", HOME=iso + "/home")
ui = subprocess.Popen([os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)
shots = []
sid = None
try:
    sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["value"]["sessionId"]
    b = BASE + "/session/" + sid
    rq("POST", b + "/window/rect", {"width": 1400, "height": 900})

    def script(js, args=None):
        return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})["value"]

    def wait_for(js, timeout=10.0, args=None):
        end = time.time() + timeout
        while time.time() < end:
            v = script(js, args)
            if v:
                return v
            time.sleep(0.15)
        return script(js, args)

    def element(css):
        return rq("POST", b + "/element", {"using": "css selector", "value": css})["value"]

    def click(css, x=0, y=0):
        rq("POST", b + "/actions", {"actions": [{"type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"}, "actions": [
            {"type": "pointerMove", "duration": 0, "origin": element(css), "x": x, "y": y},
            {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]})
        rq("DELETE", b + "/actions")

    def key(value):
        rq("POST", b + "/actions", {"actions": [{"type": "key", "id": "kb", "actions": [
            {"type": "keyDown", "value": value}, {"type": "keyUp", "value": value}]}]})
        rq("DELETE", b + "/actions")

    def shot(label):
        if not SHOTS:
            return
        os.makedirs(SHOTS, exist_ok=True)
        time.sleep(0.5)
        path = os.path.join(SHOTS, label)
        with open(path, "wb") as fh:
            fh.write(base64.b64decode(rq("GET", b + "/screenshot")["value"]))
        shots.append(path)

    def both_themes(stem):
        if SHOTS:
            for t in ("dark", "light"):
                script("document.documentElement.setAttribute('data-theme', arguments[0]);", [t])
                shot("%s-%s.png" % (stem, t))
            script("document.documentElement.setAttribute('data-theme', 'dark');")

    def on_bench(slug):
        return wait_for("return document.getElementById('bench-path').textContent.indexOf(arguments[0] + '.toml') >= 0 &&"
                        " !!document.querySelector('#bench-svg [data-part]');", 8, [slug])

    def pick_bench(slug):
        script("var s=document.getElementById('bench-pick'); s.value=arguments[0]; s.dispatchEvent(new Event('change'));", [slug])

    def tabs():
        return script("return Array.from(document.querySelectorAll('#bench-configs [data-cfg]')).map(function(t){"
                      "return [t.textContent, t.getAttribute('aria-selected') === 'true', t.hasAttribute('data-live-config')];});")

    def tab(i):
        script("document.querySelector('#bench-configs [data-cfg=\"' + arguments[0] + '\"]').click();", [i])

    def row():
        return script("function v(id){return !document.getElementById(id).hidden;}"
                      "return {row: v('bench-config-row'), add: v('bench-cfg-add'), rename: v('bench-cfg-rename'), del: v('bench-cfg-delete')};")

    def dialog_open():
        return script("return document.getElementById('bench-dialog').style.display === 'block';")

    def dialog_state():
        return script("var d=function(i){return document.getElementById(i);};"
                      "return {title: d('bench-dialog-title').textContent, value: d('bench-dialog-input').value,"
                      " check: !d('bench-dialog-check').hidden ? d('bench-dialog-check-label').textContent : null,"
                      " error: d('bench-dialog-error').hidden ? '' : d('bench-dialog-error').textContent};")

    def dialog_ok(value=None, checked=None):
        script("var i=document.getElementById('bench-dialog-input'); if (arguments[0] !== null) i.value=arguments[0];"
               "if (arguments[1] !== null) document.getElementById('bench-dialog-check-input').checked=arguments[1];"
               "document.getElementById('bench-dialog-ok').click();", [value, checked])

    ESC = "\ue00c"

    def notice():
        return script("var n=document.getElementById('bench-notice'); return n.hidden ? '' : n.textContent;")

    def dirty():
        return script("return !document.getElementById('bench-dirty').hidden;")

    def save():
        script("document.getElementById('bench-save').click();")
        wait_for("return document.getElementById('bench-dirty').hidden;", 6)
        time.sleep(0.3)

    def file(slug):
        return tomllib.loads((TOPO / (slug + ".toml")).read_text())

    def bar():
        return script("function t(id){var e=document.getElementById(id); return e.hidden ? null : e.textContent;}"
                      "return {state: t('bench-live'), make: t('bench-make-live'), again: !document.getElementById('bench-declare-again').hidden,"
                      " wt: t('bench-live-wt')};")

    def live():
        return ui_json("/api/bench/live")["live"]

    def report_text():
        return script("return document.getElementById('bench-live-body').innerText;") or ""

    vocab_rule = ui_json("/api/bench/vocabulary")["config_name"]["rule"]

    rq("POST", b + "/url", {"url": PAGE + "/#topology"})
    on_bench("cfg-bench")
    script("window.__errs=[]; addEventListener('error', function(e){ __errs.push(String(e.message)); });"
           "addEventListener('unhandledrejection', function(e){ __errs.push('unhandled: ' + String(e.reason && e.reason.message || e.reason)); });")

    # --- the config row ---------------------------------------------------------------------
    check("the config row shows the tabs and the editor's controls", tabs() == [["A", True, False]] and row() == {"row": True, "add": True, "rename": True, "del": True}, [tabs(), row()])

    # --- add: a copy of the config on screen -----------------------------------------------------
    script("document.getElementById('bench-cfg-add').click();")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    d = dialog_state()
    check("+ Config asks for a name, offering the first free letter and a copy of the config on screen",
          d["title"] == "New config" and d["value"] == "B" and d["check"] == "Copy the cables of A", d)
    both_themes("config-add-dialog")
    dialog_ok(" X", None)
    d = dialog_state()
    check("a name a host study could not write is refused in the dialog, in the vocabulary's own sentence",
          dialog_open() and d["error"] == vocab_rule[0].upper() + vocab_rule[1:] + ".", d["error"])
    dialog_ok("A", None)
    check("a name the bench already has is refused, the dialog kept", dialog_open() and "already has a config named A" in dialog_state()["error"], dialog_state())
    dialog_ok("B", True)
    wait_for("return document.getElementById('bench-dialog').style.display !== 'block';", 4)
    check("the copy lands as config B, on screen, unsaved, with A's cable",
          tabs() == [["A", False, False], ["B", True, False]] and dirty() and
          script("return document.querySelectorAll('#bench-svg .bn-cable.bn-cfg').length;") == 1, [tabs(), notice()])
    check("the notice says what was added", "Config B added, a copy of A" in notice(), notice())
    script("document.getElementById('bench-undo').click();")
    check("undo takes the new config back", [t[0] for t in tabs()] == ["A"], tabs())
    script("document.getElementById('bench-redo').click();")
    check("redo brings it again", [t[0] for t in tabs()] == ["A", "B"], tabs())

    # --- rename ---------------------------------------------------------------------------------
    tab(1)
    script("document.getElementById('bench-cfg-rename').click();")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    check("Rename asks with the current name", dialog_state()["value"] == "B" and dialog_state()["check"] is None, dialog_state())
    dialog_ok("B · copy", None)
    wait_for("return document.getElementById('bench-dialog').style.display !== 'block';", 4)
    check("the tab takes the new name", [t[0] for t in tabs()] == ["A", "B · copy"], tabs())
    save()
    f = file("cfg-bench")
    check("Save writes both configs, the copy with A's cables",
          f.get("configs") == [{"name": "A", "cables": [["an.out", "brk.usb-c"]]}, {"name": "B · copy", "cables": [["an.out", "brk.usb-c"]]}], f.get("configs"))
    both_themes("config-row")

    # --- make A live (explicit), then switch configs record-only ---------------------------------------
    tab(0)
    wait_for("var l=document.getElementById('bench-live'); return !l.hidden && l.textContent === 'not live';", 6)
    click("#bench-make-live")
    wait_for("return !document.getElementById('topo-panel-live').hidden && document.getElementById('bench-live-body').innerText.indexOf('Applied') >= 0;", 15)
    txt = report_text()
    check("an explicit make-live declares A and the record names it", (live() or {}).get("config") == "A" and bar()["state"] == "live · A", [live(), bar()])
    check("a bench board with no board type is not proposed, and says why", "pd-bench" in txt and "no board type" in txt, txt[-600:])
    key(ESC)
    tab(1)
    b1 = bar()
    check("on another config the bar offers to make it live", b1["make"] == "Make B · copy live", b1)
    writes = list(CORE_WRITES)
    click("#bench-make-live")
    wait_for("return document.getElementById('bench-live').textContent === 'live · B · copy';", 6)
    check("Make <config> live records it, and sends Core nothing", (live() or {}).get("config") == "B · copy" and CORE_WRITES == writes,
          [live(), CORE_WRITES[len(writes):]])
    check("the live tab moves, and Declare again is the way to re-apply", tabs()[1][2] and not tabs()[0][2] and bar()["again"] and bar()["make"] is None, [tabs(), bar()])

    # --- renaming the live config carries the record ---------------------------------------------
    script("document.getElementById('bench-cfg-rename').click();")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    check("renaming the live config says the record follows once saved", "live record follows" in script("return document.getElementById('bench-dialog-text').textContent;"))
    dialog_ok("C", None)
    wait_for("return document.getElementById('bench-dialog').style.display !== 'block';", 4)
    writes = len(CORE_WRITES)
    save()
    followed = wait_for("return document.getElementById('bench-live').textContent === 'live · C';", 8)
    check("a save that renames the live config moves the record to the new name", bool(followed) and (live() or {}).get("config") == "C", [live(), bar()])
    wait_for("var w=document.getElementById('bench-live-wt'); return !w.hidden && w.textContent.indexOf('declared') === 0;", 8)
    check("and then declares the saved bench, as a save of the live bench does", len(CORE_WRITES) > writes and (bar()["wt"] or "").startswith("declared") and
          "follows the rename to C" in notice(), [bar(), notice()])

    # --- deleting the live config leaves the record naming it, which the bar says ----------------------
    script("document.getElementById('bench-cfg-delete').click();")
    check("deleting the live config is never refused, and the notice says it is the live wiring",
          [t[0] for t in tabs()] == ["A"] and "live wiring" in notice(), [tabs(), notice()])
    save()
    gone = wait_for("var l=document.getElementById('bench-live'); return l.textContent === 'live config C is gone' ? l.textContent : '';", 8)
    check("once saved, the bar says the live config is gone", bool(gone) and (live() or {}).get("config_missing") is True, [bar(), live()])
    b2 = bar()
    check("and offers to make the config on screen live, record-only", b2["make"] == "Make A live" and not b2["again"], b2)
    writes = list(CORE_WRITES)
    click("#bench-make-live")
    back = wait_for("return document.getElementById('bench-live').textContent === 'live · A';", 6)
    check("which records it and sends Core nothing", bool(back) and (live() or {}).get("config") == "A" and CORE_WRITES == writes, [live(), CORE_WRITES[len(writes):]])

    # --- a bench board picks a board type, and is proposed -------------------------------------
    click('#bench-svg [data-part="partner"] .bn-body', -30, 4)
    opts = wait_for("var s=document.querySelector('#bench-inspector select[data-ed=\"board\"]');"
                    " return s && !s.disabled ? Array.from(s.options).map(function(o){return o.value;}) : null;", 6)
    check("the inspector offers a board type on a bench board, from the project's catalog",
          opts == ["", "nrf54l15dk/nrf54l15/cpuapp", "nucleo_g0b1re"], opts)
    script("var s=document.querySelector('#bench-inspector select[data-ed=\"board\"]'); s.value='nucleo_g0b1re'; s.dispatchEvent(new Event('change', {bubbles:true}));")
    check("picking one is a bench edit, unsaved until Save", dirty())
    writes = list(CORE_WRITES)
    save()
    part = next(p for p in file("cfg-bench")["parts"] if p["id"] == "partner")
    check("Save writes only the part's board, and the role stays none", part.get("board") == "nucleo_g0b1re" and "role" not in part, part)
    wait_for("var w=document.getElementById('bench-live-wt'); return !w.hidden && w.textContent.indexOf('declared') === 0;", 8)
    script("document.getElementById('bench-live-wt').click();")
    wait_for("return !document.getElementById('topo-panel-live').hidden;", 6)
    rows = script("return Array.from(document.querySelectorAll('#bench-live-body .bench-live-row')).map(function(r){"
                  " var b=r.querySelector('[data-live-confirm]'); return [b ? b.getAttribute('data-live-confirm') : null, r.innerText];});")
    mine = next((r for r in rows if r[0] is not None and NUC_PROBE in r[1]), None)
    check("the save's write-through proposes the bench board's enrolment, on its probe, as its catalog chip",
          mine is not None and "bench board" in mine[1] and "STM32G0B1RE" in mine[1], rows)
    if mine:
        script("var e=document.querySelector('#bench-live-body [data-live-confirm=\"' + arguments[0] + '\"]'); e.scrollIntoView({block:'center'}); e.click();", [mine[0]])
        end = time.time() + 8
        while time.time() < end and not any(e.get("probe_serial") == NUC_PROBE for e in ENROLLS):
            time.sleep(0.2)
        sent = next((e for e in ENROLLS if e.get("probe_serial") == NUC_PROBE), None)
        check("Confirm enrols it with no role and its label as the name", sent is not None and sent.get("role") == "" and sent.get("name") == "pd-bench" and
              sent.get("chip") == "STM32G0B1RE", sent)
    key(ESC)
    wait_for("return document.getElementById('topo-panel-live').hidden;", 4)
    # The floating inspector sits over the canvas's top right, where the
    # analyzer is: out of the way for the click, drawn again by the selection.
    script("document.getElementById('bench-inspector').hidden = true;")
    click('#bench-svg [data-part="an"] .bn-body', 0, 0)
    picked = wait_for("var t=document.querySelector('#bench-inspector .bn-ins-title'); return t && t.textContent === 'Analyzer' &&"
                      " !!document.querySelector('#bench-inspector [data-ed=\"role\"]');", 4)
    check("an analyzer, a role-less part that is no board, is offered no board type", bool(picked) and not script(
        "return !!document.querySelector('#bench-inspector select[data-ed=\"board\"]');"),
        script("var i=document.getElementById('bench-inspector'); return i.hidden ? 'inspector hidden' : i.innerText.slice(0, 120);"))

    # --- a bench with no config, and a file holding a name a study cannot write -------------------------
    pick_bench("bare-bench")
    on_bench("bare-bench")
    check("a bench with no config keeps the row, with + Config and no tab to rename or delete",
          tabs() == [] and row() == {"row": True, "add": True, "rename": False, "del": False}, [tabs(), row()])
    script("document.getElementById('bench-cfg-add').click();")
    wait_for("return document.getElementById('bench-dialog').style.display === 'block';", 4)
    check("its first config is offered as A, with nothing to copy", dialog_state()["value"] == "A" and dialog_state()["check"] is None, dialog_state())
    dialog_ok(None, None)
    wait_for("return document.querySelectorAll('#bench-configs [data-cfg]').length === 1;", 4)
    save()
    check("and Save writes it empty", file("bare-bench").get("configs") == [{"name": "A", "cables": []}], file("bare-bench").get("configs"))

    pick_bench("odd-bench")
    on_bench("odd-bench")
    click('#bench-svg [data-part="spare"] .bn-body', -30, 4)
    key("f")
    wait_for("return !document.getElementById('bench-dirty').hidden;", 4)
    script("document.getElementById('bench-save').click();")
    said = wait_for("var n=document.getElementById('bench-notice'); return !n.hidden && n.textContent.indexOf('Not saved') === 0 ? n.textContent : '';", 6)
    check("a save of a file holding a name a study cannot write shows the server's refusal verbatim",
          said == "Not saved: config ` A` cannot be saved: " + vocab_rule and dirty(), said)

    check("no uncaught JS error or unhandled rejection", script("return window.__errs || [];") == [], script("return window.__errs || [];"))
    if shots:
        print("screenshots: " + " ".join(shots))
finally:
    if sid:
        try:
            urllib.request.urlopen(urllib.request.Request(BASE + "/session/" + sid, method="DELETE"), timeout=10)
        except Exception:
            pass
    ui.terminate()
    core.shutdown()
    shutil.rmtree(iso, ignore_errors=True)

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
