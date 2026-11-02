"""The operator prompt drawing a bench change, in a real browser (the bench
editor's phase 5, `embarch-ui` decision 69's modal).

A host study's `Prompt` may name a bench config (`embarch-study-designer`
decision 88, forwarded by Core as `bench_config`, `embarch-core` decision 107).
This drives the three cases the modal resolves it in, against `stub_core.py`'s
prompting host study and the UI's real bench and live routes on a scratch repo:

- **the live bench has the config**: the change from the live config is drawn
  (the canvas's renderer) and listed — cables to unplug and to plug, by part
  and connector label — and Confirm answers the prompt, then records that
  config live through `POST /api/bench/live/config` (no Core call);
- **it lacks the config**: "config C isn't on this bench", the bench's configs
  to pick and the text alone; a pick is drawn, Confirm answers and records the
  pick, and the run keeps the pick for the next prompt naming the same config;
  the text alone only answers;
- **no live bench, another repo's, or one that cannot be read**: the text alone
  and one line why.

And: a slow bench lookup never holds the answer (Confirm answers at once, and
the mark lands once the lookup does); a refused mark is said while the answer
stands; a prompt naming no config shows no bench section.

The browser opens a proxy in front of the UI that can delay `GET
/api/bench/live` and refuse `POST /api/bench/live/config`; everything else,
the prompt stream included, passes through. Ports: geckodriver at
EMBARCH_GECKO_PORT; the UI at EMBARCH_UI_PORT, the proxy at +1, stub_core.py
at +2. EMBARCH_BENCH_SHOTS=<dir> saves screenshots in both themes.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_prompt_bench.py
"""
import base64, http.client, json, os, shutil, subprocess, sys, tempfile, threading, time, urllib.request
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

HERE = Path(__file__).resolve().parent
UI_PORT = addr.ui_port(4879)
BASE = addr.gecko_url()
PROXY_PORT = int(UI_PORT) + 1
STUB_PORT = int(UI_PORT) + 2
PAGE = "http://127.0.0.1:%d" % PROXY_PORT
STUB = "http://127.0.0.1:%d" % STUB_PORT
SHOTS = os.environ.get("EMBARCH_BENCH_SHOTS")

PD_DUT = """id = "pd-dut"
label = "PD DUT"
category = "board"
look = "power-board"

[[connectors]]
id = "usb"
label = "USB"
kind = "usb-host"
side = "left"
functions = [{ function = "probe" }]

[[connectors]]
id = "usb-c"
label = "USB-C port"
kind = "usb-c"
side = "right"
"""

BENCH = """name = "pd-bench"
saved_at_utc_ms = 3

[[parts]]
id = "host"
template = "this-machine"
at = [20, 110]

[[parts]]
id = "dut"
template = "pd-dut"
role = "dut"
at = [340, 60]
usb = { usb = { probe = "DUT1" } }

[[parts]]
id = "an"
template = "cy4500"
label = "Analyzer"
at = [700, 60]

[[parts]]
id = "brk"
template = "usb-c-breakout"
at = [1040, 60]

[[cables]]
from = "dut.usb"
to = "host"

[[configs]]
name = "A"
cables = [["dut.usb-c", "brk.usb-c"]]

[[configs]]
name = "B"
cables = [["dut.usb-c", "an.in"], ["an.out", "brk.usb-c"]]
"""

FAULT = {"delay_live": 0.0, "refuse_mark": False, "delay_mark": 0.0}
MARKS = []

LISTEN = (b"<script>window.__errs=[];addEventListener('error',function(e){__errs.push(String(e.message))});"
          b"addEventListener('unhandledrejection',function(e){__errs.push('unhandled: '+String(e.reason&&e.reason.message||e.reason))});</script>")


class Proxy(BaseHTTPRequestHandler):
    """Everything to the UI, the event streams included; the two bench routes
    the prompt reads and writes can be slowed or refused."""
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype="text/plain", headers=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _forward(self):
        path = urlparse(self.path).path
        length = int(self.headers.get("content-length") or 0)
        data = self.rfile.read(length) if length else None
        if path == "/api/bench/live" and self.command == "GET" and FAULT["delay_live"]:
            time.sleep(FAULT["delay_live"])
        if path == "/api/bench/live/config" and self.command == "POST":
            MARKS.append(json.loads(data or b"{}"))
            if FAULT["delay_mark"]:
                time.sleep(FAULT["delay_mark"])
            if FAULT["refuse_mark"]:
                return self._send(409, b"this repo's bench is not live: another repo went live later", headers={"x-embarch-refusal": "superseded"})
        up = http.client.HTTPConnection("127.0.0.1", int(UI_PORT), timeout=120)
        drop = {"connection", "keep-alive", "accept-encoding", "content-length"}
        if path == "/":
            drop.add("if-none-match")
        headers = {k: v for k, v in self.headers.items() if k.lower() not in drop}
        try:
            up.request(self.command, self.path, body=data, headers=headers)
            resp = up.getresponse()
        except OSError as e:
            return self._send(502, str(e).encode())
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
        return json.loads(r.read().decode() or "{}")


def wd(method, path, body=None):
    return rq(method, BASE + path, body)["value"]


def stub(op, body=None):
    return rq("POST", STUB + "/_stub/prompt/" + op, body or {})


def ui_json(method, path, body=None):
    return rq(method, "http://127.0.0.1:%s%s" % (UI_PORT, path), body)


fails = []


def check(label, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + label + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(label)


iso = tempfile.mkdtemp(prefix="embarch-ui-prompt-bench-")
fw = Path(iso) / "fw"
(fw / ".git").mkdir(parents=True)
(fw / "embarch" / "topologies").mkdir(parents=True)
(fw / "embarch" / "parts").mkdir(parents=True)
(fw / "embarch" / "parts" / "pd-dut.toml").write_text(PD_DUT)
(fw / "embarch" / "topologies" / "pd-bench.toml").write_text(BENCH)
RECORD = Path(iso) / "live-benches.json"
KEY = os.path.realpath(fw)


def record(config, machine_repo=None, raw=None):
    """The live record as the UI keeps it (bench::live's strict shape)."""
    if raw is not None:
        RECORD.write_text(raw)
        return
    if config is False:
        RECORD.unlink(missing_ok=True)
        return
    rec = {"repos": {KEY: {"slug": "pd-bench", "config": config, "since_utc_ms": 1}},
           "machine": {"repo": machine_repo or KEY, "slug": "pd-bench" if not machine_repo else "other-bench", "config": config, "since_utc_ms": 2}}
    RECORD.write_text(json.dumps(rec))


record("A")
cfg = Path(iso) / "ui.toml"
cfg.write_text('[core]\nbase_url = "%s"\ntoken = "stub-token"\n\n[study_designer]\nfirmware_repo_path = "%s"\n' % (STUB, fw))
stub_proc = subprocess.Popen([sys.executable, str(HERE / "stub_core.py"), str(STUB_PORT)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(0.8)
env = dict(os.environ, EMBARCH_UI_CONFIG=str(cfg), EMBARCH_UI_PORT=UI_PORT, EMBARCH_DESK_PORT=str(int(UI_PORT) + 3),
           EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data", XDG_CONFIG_HOME=iso + "/config",
           EMBARCH_UI_STATE=iso + "/recent-projects.json", HOME=iso + "/home")
ui = subprocess.Popen([os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")], env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
proxy = serve(PROXY_PORT, Proxy)
time.sleep(2)
shots = []
sid = None
try:
    stub("reset")
    sid = wd("POST", "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
    b = "/session/" + sid
    wd("POST", b + "/window/rect", {"width": 1280, "height": 960})

    def script(js, args=None):
        return wd("POST", b + "/execute/sync", {"script": js, "args": args or []})

    def wait_for(js, timeout=12.0, args=None):
        end = time.time() + timeout
        while time.time() < end:
            v = script(js, args)
            if v:
                return v
            time.sleep(0.15)
        return script(js, args)

    def shot(label):
        if not SHOTS:
            return
        os.makedirs(SHOTS, exist_ok=True)
        time.sleep(0.5)
        path = os.path.join(SHOTS, label)
        with open(path, "wb") as fh:
            fh.write(base64.b64decode(wd("GET", b + "/screenshot")))
        shots.append(path)

    def both_themes(stem):
        if SHOTS:
            for t in ("dark", "light"):
                script("document.documentElement.setAttribute('data-theme', arguments[0]);", [t])
                shot("%s-%s.png" % (stem, t))
            script("document.documentElement.setAttribute('data-theme', 'dark');")

    def modal():
        return script(
            "function t(i){var e=document.getElementById(i);return e?e.textContent:'';}"
            "function shown(i){var e=document.getElementById(i);return !!e&&e.style.display!=='none'&&!e.hidden;}"
            "var bx=document.getElementById('prompt-bench');"
            "return {open:shown('prompt-dialog'), text:t('prompt-text'), result:shown('prompt-result')?t('prompt-result'):'',"
            " confirm:shown('prompt-confirm'), close:shown('prompt-close'), confirmDisabled:document.getElementById('prompt-confirm').disabled,"
            " bench:!bx.hidden, state:bx.getAttribute('data-state'), head:t('prompt-bench-head'),"
            " unplug:Array.from(document.querySelectorAll('#prompt-bench-lists .prompt-unplug li')).map(function(l){return l.textContent;}),"
            " plug:Array.from(document.querySelectorAll('#prompt-bench-lists .prompt-plug li')).map(function(l){return l.textContent;}),"
            " svg:!document.getElementById('prompt-bench-svg').hasAttribute('hidden'),"
            " drawnPlug:document.querySelectorAll('#prompt-bench-svg [data-change=\"plug\"]').length,"
            " drawnUnplug:document.querySelectorAll('#prompt-bench-svg [data-change=\"unplug\"]').length,"
            " drawnParts:document.querySelectorAll('#prompt-bench-svg [data-part]').length,"
            " picks:Array.from(document.querySelectorAll('#prompt-bench-pick input')).map(function(r){return [r.value, r.checked];})};")

    def wait_modal(pred, secs=14):
        end = time.time() + secs
        m = modal()
        while time.time() < end:
            m = modal()
            if pred(m):
                return m
            time.sleep(0.2)
        return m

    def live_config():
        return ui_json("GET", "/api/bench/live")["live"]["config"]

    def answers():
        return stub("answers")["answers"]

    def click(css):
        script("document.querySelector(arguments[0]).click();", [css])

    def real_click(css):
        """A WebDriver click, which moves focus as a person's does."""
        el = wd("POST", b + "/element", {"using": "css selector", "value": css})
        wd("POST", b + "/element/" + list(el.values())[0] + "/click", {})

    wd("POST", b + "/url", {"url": PAGE + "/#debug"})
    time.sleep(1.5)

    # --- 1. the live bench has the config: the change is drawn and listed ------------------
    stub("start", {"prompt_id": 1, "text": "Put the analyzer in line", "bench_config": "B", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "has")
    print("info  modal:", m)
    check("a prompt naming a config the live bench has opens with the bench section", m["open"] and m["bench"] and m["state"] == "has", m)
    check("the head says the bench and the configs, from the live one to the named one",
          "Re-cable pd-bench to config B" in m["head"] and "from config A" in m["head"], m["head"])
    check("the cable to unplug is named by part label and connector label",
          m["unplug"] == ["PD DUT · USB-C port  ↔  USB-C breakout · USB-C"], m["unplug"])
    check("the cables to plug are named the same way, the analyzer's two USB-C told apart by id",
          sorted(m["plug"]) == sorted(["PD DUT · USB-C port  ↔  Analyzer · USB-C (in)",
                                       "Analyzer · USB-C (out)  ↔  USB-C breakout · USB-C"]), m["plug"])
    check("the change is drawn with the canvas's renderer: the parts, two cables to plug and one to unplug",
          m["svg"] and m["drawnParts"] == 4 and m["drawnPlug"] == 2 and m["drawnUnplug"] == 1, m)
    check("Confirm is live from the first frame", m["confirm"] and not m["confirmDisabled"], m)
    both_themes("prompt-change")
    click("#prompt-confirm")
    m = wait_modal(lambda m: not m["open"])
    a = answers()
    check("Confirm answers the prompt as ui", a and a[-1].get("prompt_id") == 1 and a[-1].get("answer") == "confirmed" and a[-1].get("answered_by") == "ui", a)
    check("and then records config B live, naming the bench", wait_for("return true;") and (time.sleep(0.6) or live_config() == "B") and
          MARKS[-1:] == [{"slug": "pd-bench", "config": "B"}], [live_config(), MARKS])
    check("the modal closes on its own answer", not m["open"], m)

    # --- 2. the live bench lacks the config: pick one, or the text alone ------------------
    stub("start", {"prompt_id": 2, "text": "Wire the sink-only setup", "bench_config": "C", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "missing")
    print("info  modal:", m)
    check("a config the bench lacks says so, naming it", "Config C isn't on this bench" in m["head"], m["head"])
    check("it offers the bench's configs and the text alone, the text alone picked", m["picks"] == [["A", False], ["B", False], ["", True]], m["picks"])
    check("with the text alone nothing is drawn", not m["svg"] and not m["unplug"] and not m["plug"], m)
    real_click('#prompt-bench-pick input[value="A"]')
    m = wait_modal(lambda m: m["svg"])
    check("a pick draws the change from the live config to it", m["svg"] and m["unplug"] and m["plug"] and
          m["unplug"][0].startswith("PD DUT") and m["drawnUnplug"] == 2 and m["drawnPlug"] == 1, m)
    check("the pick keeps its radio (and its focus): the list is not rebuilt", script(
        "return document.activeElement && document.activeElement.value === 'A' && document.activeElement.checked;"))
    both_themes("prompt-missing")
    click("#prompt-confirm")
    m = wait_modal(lambda m: not m["open"])
    a = answers()
    time.sleep(0.6)
    check("Confirm with a pick answers the prompt", a[-1].get("prompt_id") == 2 and a[-1].get("answer") == "confirmed", a[-1:])
    check("and marks the picked config live for the run", live_config() == "A" and MARKS[-1] == {"slug": "pd-bench", "config": "A"}, [live_config(), MARKS[-1:]])

    stub("start", {"prompt_id": 3, "text": "Wire the sink-only setup again", "bench_config": "C", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "missing")
    check("the run keeps the pick: the next prompt naming C has A picked, and says so",
          m["picks"] == [["A", True], ["B", False], ["", False]] and "picked for it earlier in this run" in m["head"], m)
    script("document.querySelector('#prompt-bench-pick input[value=\"\"]').click();")
    marks = len(MARKS)
    click("#prompt-confirm")
    m = wait_modal(lambda m: not m["open"])
    time.sleep(0.6)
    check("the text alone just answers: no mark, the record as it was",
          answers()[-1].get("prompt_id") == 3 and len(MARKS) == marks and live_config() == "A", [answers()[-1:], MARKS[marks:]])

    # The text alone replaces the run's pick: the next prompt naming C has it
    # chosen, and a Confirm while the lookup is on its way marks nothing.
    FAULT["delay_live"] = 2.0
    stub("start", {"prompt_id": 31, "text": "Sink-only, third time", "bench_config": "C", "deadline_in_ms": 120000})
    wait_modal(lambda m: m["open"] and m["state"] == "loading", 8)
    click("#prompt-confirm")
    wait_modal(lambda m: not m["open"], 6)
    FAULT["delay_live"] = 0.0
    time.sleep(2.8)
    check("the run keeps the text alone: a Confirm made while the lookup was on its way marks nothing",
          answers()[-1].get("prompt_id") == 31 and len(MARKS) == marks and live_config() == "A", [answers()[-1:], MARKS[marks:]])
    stub("start", {"prompt_id": 32, "text": "Sink-only, fourth time", "bench_config": "C", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "missing")
    check("and the next prompt naming C has the text alone chosen, saying so",
          m["picks"] == [["A", False], ["B", False], ["", True]] and "the text alone was chosen for it earlier in this run" in m["head"], m)
    click("#prompt-cancel")
    wait_modal(lambda m: not m["open"])

    # --- 3. a slow lookup never holds the answer -----------------------------------------
    FAULT["delay_live"] = 3.0
    stub("start", {"prompt_id": 4, "text": "Analyzer back in line", "bench_config": "B", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "loading", 8)
    check("while the live bench is read the modal says so, and Confirm is live", m["state"] == "loading" and "reading the live bench" in m["head"] and
          not m["confirmDisabled"], m)
    t0 = time.time()
    click("#prompt-confirm")
    m = wait_modal(lambda m: not m["open"], 6)
    took = time.time() - t0
    a = answers()
    check("Confirm answers at once, the lookup still on its way", a[-1].get("prompt_id") == 4 and took < 2.5 and not m["open"], [took, a[-1:]])
    check("nothing is marked before the lookup lands", live_config() == "A")
    FAULT["delay_live"] = 0.0
    landed = wait_for("return true;", 0.1) and (time.sleep(3.5) or True)
    check("once it lands, the confirmed config is marked live", live_config() == "B" and MARKS[-1] == {"slug": "pd-bench", "config": "B"}, [live_config(), MARKS[-1:]])

    # --- 4. a mark the record refuses is said; the answer stands -----------------------------
    FAULT["refuse_mark"] = True
    stub("start", {"prompt_id": 5, "text": "Direct cable again", "bench_config": "A", "deadline_in_ms": 120000})
    wait_modal(lambda m: m["open"] and m["state"] == "has")
    click("#prompt-confirm")
    m = wait_modal(lambda m: "not marked live" in m["result"] and m["close"])
    check("a refused mark keeps the modal, saying the answer stands and why the config was not marked",
          m["open"] and "the answer stands" in m["result"] and "another repo went live later" in m["result"] and m["close"] and not m["confirm"], m)
    check("the answer reached Core all the same", answers()[-1].get("prompt_id") == 5 and answers()[-1].get("answer") == "confirmed", answers()[-1:])
    FAULT["refuse_mark"] = False
    click("#prompt-close")

    # --- 4b. Core's resolve frame before the answer's own reply -----------------------------
    stub("answer_delay", {"ms": 1200})
    FAULT["refuse_mark"] = True
    stub("start", {"prompt_id": 6, "text": "Analyzer in line, frame first", "bench_config": "B", "deadline_in_ms": 120000})
    wait_modal(lambda m: m["open"] and m["state"] == "has")
    click("#prompt-confirm")
    m = wait_modal(lambda m: m["close"] and not m["confirm"], 4)
    check("frame first: the frame ends the prompt on screen while the answer is on its way, Close offered", m["open"] and m["close"], m)
    m = wait_modal(lambda m: "not marked live" in m["result"], 6)
    check("frame first: a refused mark is still said in that prompt's own modal, the answer standing",
          m["open"] and "the answer stands" in m["result"] and answers()[-1].get("prompt_id") == 6 and m["close"], m)
    FAULT["refuse_mark"] = False
    click("#prompt-close")
    stub("start", {"prompt_id": 7, "text": "Analyzer in line, frame first again", "bench_config": "B", "deadline_in_ms": 120000})
    wait_modal(lambda m: m["open"] and m["state"] == "has")
    click("#prompt-confirm")
    m = wait_modal(lambda m: not m["open"], 6)
    time.sleep(0.4)
    check("frame first: a mark that lands closes the modal, the config marked", not m["open"] and live_config() == "B" and
          MARKS[-1] == {"slug": "pd-bench", "config": "B"}, [m, live_config()])
    stub("answer_delay", {"ms": 0})

    # --- 4c. a prompt that arrives while a mark is on its way shows at once --------------------
    FAULT["delay_mark"] = 2.5
    FAULT["refuse_mark"] = True
    stub("start", {"prompt_id": 8, "text": "Direct cable, slow record", "bench_config": "A", "deadline_in_ms": 120000})
    wait_modal(lambda m: m["open"] and m["state"] == "has")
    click("#prompt-confirm")
    wait_modal(lambda m: m["close"], 4)
    stub("start", {"prompt_id": 9, "text": "Press the reset button", "deadline_in_ms": 120000})
    t0 = time.time()
    m = wait_modal(lambda m: m["open"] and "reset button" in m["text"], 6)
    check("a prompt arriving while the last one's mark is on its way shows at once, not after it",
          "reset button" in m["text"] and time.time() - t0 < 2.0 and m["confirm"], [time.time() - t0, m])
    note = wait_for("var n=document.getElementById('prompt-mark-note'); return !n.hidden ? n.textContent : '';", 6)
    check("and the mark's refusal is said on that prompt's own line, naming the earlier step",
          "An earlier prompt (step 'swap the cable') was confirmed" in (note or "") and "not marked live" in (note or "") and
          modal()["text"] == "Press the reset button" and not modal()["result"], [note, modal()["result"]])
    FAULT["delay_mark"] = 0.0
    FAULT["refuse_mark"] = False
    click("#prompt-confirm")
    wait_modal(lambda m: not m["open"])

    # --- 4d. a project switch while a prompt is up looks the bench up again ---------------------
    record("A")
    stub("start", {"prompt_id": 10, "text": "Analyzer in line, after a switch", "bench_config": "B", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "has")
    check("before the switch: the change from A is drawn", "from config A" in m["head"], m["head"])
    record("B")
    script("document.dispatchEvent(new CustomEvent('embarch:project', {detail: {path: arguments[0]}}));", [str(fw)])
    m = wait_modal(lambda m: "already" in m["head"], 8)
    check("a project switch mid-prompt reads the live bench again (now wired as B)", "wired as config B already" in m["head"], m["head"])

    # --- 4e. the already-wired case still records, and a config_missing record ---------------------
    marks = len(MARKS)
    click("#prompt-confirm")
    wait_modal(lambda m: not m["open"])
    time.sleep(0.5)
    check("already wired: Confirm still sends the config, the record keeping it", MARKS[marks:] == [{"slug": "pd-bench", "config": "B"}] and
          live_config() == "B", MARKS[marks:])
    record("Z")
    stub("start", {"prompt_id": 11, "text": "Analyzer in line, from a gone config", "bench_config": "B", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and m["state"] == "has")
    check("a live config gone from the bench: what to unplug cannot be told, every cable of B is to plug",
          "the live config Z is not on the bench any more" in m["head"] and m["unplug"] == ["nothing"] and len(m["plug"]) == 2 and
          m["drawnPlug"] == 2 and m["drawnUnplug"] == 0, m)
    click("#prompt-confirm")
    wait_modal(lambda m: not m["open"])
    time.sleep(0.5)
    check("and Confirm records B, which the bench has", live_config() == "B", live_config())

    # --- 5. no live bench, another repo's, unreadable: the text alone and one line why -----------
    for label, setup, words in (
            ("no live bench", lambda: record(False), "no bench is live in this repo"),
            ("another repo holds the machine", lambda: record("A", machine_repo="/elsewhere/other-repo"), "not the machine's live one"),
            ("the record cannot be read", lambda: record(None, raw="{ not json"), "could not be read")):
        setup()
        pid = 10 + len(answers())
        stub("start", {"prompt_id": pid, "text": "Check the cable", "bench_config": "B", "deadline_in_ms": 120000})
        m = wait_modal(lambda m: m["open"] and m["state"] == "none")
        check("%s: the text alone and one line why" % label, m["bench"] and words in m["head"] and "the text alone" in m["head"] and
              not m["svg"] and not m["picks"] and m["text"] == "Check the cable", m["head"])
        marks = len(MARKS)
        click("#prompt-confirm")
        wait_modal(lambda m: not m["open"])
        check("%s: Confirm just answers" % label, answers()[-1].get("prompt_id") == pid and len(MARKS) == marks, answers()[-1:])
    record("A")

    # --- 6. a prompt naming no config shows no bench section ---------------------------------
    stub("start", {"prompt_id": 30, "text": "Press the button", "deadline_in_ms": 120000})
    m = wait_modal(lambda m: m["open"] and "button" in m["text"])
    check("a prompt naming no config shows no bench section", m["open"] and not m["bench"], m)
    click("#prompt-confirm")
    wait_modal(lambda m: not m["open"])
    check("no uncaught JS error or unhandled rejection", script("return window.__errs || [];") == [], script("return window.__errs || [];"))
    if shots:
        print("screenshots: " + " ".join(shots))
finally:
    try:
        stub("reset")
    except Exception:
        pass
    if sid:
        try:
            wd("DELETE", "/session/" + sid)
        except Exception:
            pass
    ui.terminate()
    stub_proc.terminate()
    proxy.shutdown()
    shutil.rmtree(iso, ignore_errors=True)

print(("\n%d check(s) failed: " % len(fails)) + ", ".join(fails) if fails else "\nall checks passed")
sys.exit(1 if fails else 0)
