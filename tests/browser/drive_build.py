"""The Build card and the Debug tab's `builds` source, in a real browser.

`drive.py`'s sibling, and deliberately a second script rather than more
checks inside it: that one drives the Live Study tab against `stub_core.py`'s
fixtures, and these two surfaces need neither a stub nor a study. What they
do need is the **unavailable** path, which is the one a fresh checkout
actually has — no firmware repo open, no `embarch-api` config, no build ever
run — and which is where the card has to say why rather than show a dead
toggle or sit on a placeholder that reads as a hung request.

**Both paths are driven, and which one runs is decided by asking the server**
rather than by a flag: the survey says whether this bench can build, and the
available half is skipped with a line saying so where it cannot. That keeps
one script correct on a fresh checkout and on a configured bench, instead of
a second script nobody runs on the machine that has the workspace.

Run it the same way as `drive.py`:

    cargo build --release
    geckodriver --port 4444 &
    EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
    python3 tests/browser/drive_build.py
"""
import json, os, urllib.request, urllib.error, time, sys, subprocess, tempfile, threading, stat
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# `drive.py`'s harness port by default; point it at a real instance (4890)
# to drive the available path against a bench's own workspace.
UI = addr.ui_url()
BASE = addr.gecko_url()
def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]
fails = []
def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond: fails.append(name)

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
    "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid
def script(js, args=None):
    return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})
try:
    rq("POST", b + "/url", {"url": UI + "/"})
    script("window.__errs=[];window.addEventListener('error',function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)

    # --- the Build card ---------------------------------------------------
    script("document.querySelector('.nav-item[data-tab=\"study-designer\"]').click();")
    time.sleep(3)
    check("the Build card exists", script("return !!document.getElementById('sd-build-card');"))
    note = script("return document.getElementById('sd-build-note').textContent;")
    check("the card never sits on its loading placeholder",
          "checking what this bench" not in note, note[:120])
    check("the body stays hidden while the toggle is off",
          script("return document.getElementById('sd-build-body').style.display === 'none';"))

    # --- the Debug tab's builds source ------------------------------------
    # --- the Build card, available -----------------------------------------
    #
    # Only where this bench can actually build. The survey is the authority
    # on that, so it is asked rather than guessed at from config.
    with urllib.request.urlopen(UI + "/api/build/survey", timeout=30) as r:
        survey = json.loads(r.read().decode())
    if not survey.get("available"):
        print("SKIP  the available path — this bench has no buildable project open")
    else:
        note = script("return document.getElementById('sd-build-note').textContent;")
        check("the card names the matched project and its config",
              survey["project"] in note and "embarch.toml" in note, note[:110])
        check("the toggle is enabled on a buildable bench",
              script("return document.getElementById('sd-build-on').disabled===false;"))

        script("var t=document.getElementById('sd-build-on');t.checked=true;"
               "t.dispatchEvent(new Event('change'));")
        time.sleep(1)
        check("ticking it reveals the window",
              script("return document.getElementById('sd-build-body').style.display!=='none';"))

        boards = script("return Array.from(document.getElementById('sd-build-board')"
                        ".options).map(o=>o.value);")
        check("the board picker is filled from the live scan", len(boards) > 1 and boards[0] == "",
              boards[:6])
        check("the first option on each axis is the project's own default",
              script("return document.getElementById('sd-build-board')"
                     ".options[0].textContent;") == "(the project's default)")

        # Snippets are per app, so the pool is empty until one is chosen.
        check("the snippet pool asks for an app before offering anything",
              "pick an app" in script("return document.getElementById"
                                      "('sd-build-snippet-pool').textContent;"))
        by_app = (survey.get("targets") or {}).get("snippets_by_app") or {}
        app = next((a for a, ss in by_app.items() if len(ss) >= 2), None)
        if app is None:
            print("SKIP  the ordered snippet list — no app in this repo declares two snippets")
        else:
            script("var a=document.getElementById('sd-build-app');a.value=arguments[0];"
                   "a.dispatchEvent(new Event('change'));", [app])
            time.sleep(1)
            pool = script("return Array.from(document.querySelectorAll"
                          "('#sd-build-snippet-pool .chip')).map(c=>c.textContent.trim());")
            check("choosing an app offers exactly what it declares",
                  len(pool) == len(by_app[app]), f"{len(pool)} vs {len(by_app[app])}")

            first, second = by_app[app][0], by_app[app][1]
            script("var c=Array.from(document.querySelectorAll('#sd-build-snippet-pool .chip'));"
                   "c.find(x=>x.textContent.trim()==='+ '+arguments[0]).click();"
                   "c.find(x=>x.textContent.trim()==='+ '+arguments[1]).click();", [first, second])
            time.sleep(1)
            chosen = script("return Array.from(document.querySelectorAll"
                            "('#sd-build-snippets .chip')).map(c=>c.textContent.trim());")
            check("chosen snippets are numbered in the order they were added",
                  len(chosen) == 2 and chosen[0].startswith("1. " + first)
                  and chosen[1].startswith("2. " + second), chosen)

            # **The whole point of the control.** West applies -S in order and
            # reversals row 109 is the case where that order decides whether
            # the image works, so a picker that cannot reorder is a picker
            # that lies.
            script("document.querySelectorAll('#sd-build-snippets .chip')[1]"
                   ".querySelectorAll('a')[0].click();")
            time.sleep(1)
            after = script("return Array.from(document.querySelectorAll"
                           "('#sd-build-snippets .chip')).map(c=>c.textContent.trim());")
            check("move-up actually reorders, and renumbers",
                  after[0].startswith("1. " + second) and after[1].startswith("2. " + first), after)

        rows = script("return Array.from(document.querySelectorAll"
                      "('#sd-build-flags .req-row .req-label')).map(e=>e.textContent);")
        check("one mode row per served header flag",
              rows == survey["flags"], rows)
        states = script("return Array.from(document.querySelectorAll"
                        "('input[name=\"sd-build-flag-trace_self\"]')).map(r=>r.value);")
        check("each flag offers don't-care / set / clear", states == ["", "set", "clear"], states)
        check("don't care is the default",
              script("return document.querySelector"
                     "('input[name=\"sd-build-flag-trace_self\"]').checked===true;"))

    script("document.querySelector('.nav-item[data-tab=\"debug\"]').click();")
    time.sleep(1)
    check("the builds chip exists",
          script("return !!document.querySelector('.chip[data-log-source=\"builds\"]');"))
    check("the build picker is hidden while another source is selected",
          script("return document.getElementById('log-build-pick').style.display === 'none';"))
    script("document.querySelector('.chip[data-log-source=\"builds\"]').click();")
    time.sleep(2)
    check("switching to builds shows the picker",
          script("return document.getElementById('log-build-pick').style.display !== 'none';"))
    check("the subtitle follows the source",
          "build" in script("return document.getElementById('debug-subtitle').textContent;").lower(),
          script("return document.getElementById('debug-subtitle').textContent;"))
    check("an empty build directory reads as 'none yet', not as an error",
          script("return document.getElementById('log-build-pick').textContent;").strip() == "no builds yet",
          script("return document.getElementById('log-build-pick').textContent;"))
    check("no error card is shown for a bench that has never built",
          script("return document.getElementById('debug-error').style.display === 'none';"))
    script("document.querySelector('.chip[data-log-source=\"core\"]').click();")
    time.sleep(1)
    check("switching back hides the picker again",
          script("return document.getElementById('log-build-pick').style.display === 'none';"))


    # --- a build run: the board line, in the stored log (decision 62) --------
    #
    # A real run through a second, isolated UI: a scratch firmware repo, a
    # stub `west` (no toolchain, no client workspace) and a stub Core that
    # holds a DUT role and answers flash/reset. Never the live desk, a live
    # Core or the owner's UI: scratch ports, desk config and XDG dirs.
    RUN_PORT = os.environ.get("EMBARCH_BUILD_RUN_PORT", "4895")
    CORE_PORT = int(os.environ.get("EMBARCH_BUILD_STUB_PORT", "4894"))
    TOKEN = "stub-token"
    iso = tempfile.mkdtemp(prefix="embarch-ui-build-iso-")
    repo = iso + "/repo"
    bd = repo + "/boards/acme/plank"
    os.makedirs(bd); os.makedirs(repo + "/app/blinky"); os.makedirs(repo + "/embarch")
    open(repo + "/app/blinky/CMakeLists.txt", "w").close()
    with open(bd + "/board.yml", "w") as f:
        f.write("board:\n  name: plank\n  socs:\n    - name: nrf54l15\n"
                "      variants:\n        - name: ns\n          cpucluster: cpuapp\n"
                '  revision:\n    default: "1"\n    revisions:\n      - name: "1"\n      - name: "2"\n')
    for n in ["plank_nrf54l15_cpuapp.dts", "plank_nrf54l15_cpuapp_ns.dts",
              "plank_nrf54l15_cpuapp_2.overlay", "plank_nrf54l15_cpuapp_ns_2.overlay"]:
        open(bd + "/" + n, "w").close()
    # A catalog: the DUT role's board type builds as plank / ns / 2.
    with open(repo + "/embarch/boards.toml", "w") as f:
        f.write('[[boards]]\nname = "rev6"\nbuild_target = "plank"\nvariant = "ns"\nrevision = "2"\n')
    west = iso + "/west"
    with open(west, "w") as f:
        f.write("#!/usr/bin/env python3\nimport sys, os\n"
                "a = sys.argv\nd = a[a.index('-d') + 1]\n"
                "print('stub west: building ' + ' '.join(a[1:]))\n"
                "os.makedirs(d + '/zephyr', exist_ok=True)\n"
                "open(d + '/zephyr/zephyr.hex', 'w').write(':00000001FF')\n"
                "print('stub west: done')\n")
    os.chmod(west, os.stat(west).st_mode | stat.S_IEXEC)
    with open(repo + "/embarch/embarch.toml", "w") as f:
        f.write('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n\n[[projects]]\n'
                'name = "plank"\nsource_path = "%s"\ndiscovery = "zephyr-west"\n'
                'flash_format = "hex"\nwest_binary = "%s"\nbuild_dir_root = "%s/build"\n'
                % (CORE_PORT, TOKEN, repo, west, repo))
    for cmd in (["git", "init", "-q"], ["git", "add", "-A"],
                ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"]):
        subprocess.run(cmd, cwd=repo, check=True, stdout=subprocess.DEVNULL)
    ENROLLED = [{"probe_serial": "S1", "role": "dut", "name": "rev6", "chip": "nRF54L15",
                 "hardware_id": "aa:bb", "confirmed_at_utc_ms": 1758240000000, "link_serial": None}]
    CORE = {"flash_fails": False, "flashed": 0, "resets": 0}

    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        def log_message(self, *a): pass
        def _send(self, obj, code=200):
            body = (json.dumps(obj) if not isinstance(obj, str) else obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers(); self.wfile.write(body)
        def do_GET(self):
            p = self.path.split("?")[0]
            if p == "/status": return self._send({"status": "ok", "probes": []})
            if p == "/probes/enrolled": return self._send(ENROLLED)
            if p in ("/alerts", "/signals", "/studies", "/serial-ports"): return self._send([])
            if p == "/logs/recent": return self._send({"lines": []})
            return self._send("not found", 404)
        def do_POST(self):
            n = int(self.headers.get("content-length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}") if n else {}
            p = self.path.split("?")[0]
            if p == "/resolve-chip": return self._send({"chip": "nRF54L15"})
            if p == "/flash":
                if CORE["flash_fails"]: return self._send("probe not found", 500)
                CORE["flashed"] += 1
                return self._send({"flashed": True, "chip": "nRF54L15"})
            if p == "/reset":
                CORE["resets"] += 1
                return self._send({"reset": True})
            # The study submit that follows a build is not what this checks.
            return self._send("the stub does not take studies", 500)
    srv = ThreadingHTTPServer(("127.0.0.1", CORE_PORT), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    env = dict(os.environ, EMBARCH_UI_CONFIG=repo + "/embarch/embarch.toml",
               EMBARCH_UI_PORT=RUN_PORT, EMBARCH_DESK_PORT="4893",
               EMBARCH_DESK_CONFIG_DIR=iso + "/desk-cfg", XDG_DATA_HOME=iso + "/data",
               XDG_CONFIG_HOME=iso + "/config", HOME=iso)
    ui_bin = os.environ.get("EMBARCH_UI_BIN", "./target/release/embarch-ui")
    run_ui = subprocess.Popen([ui_bin], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    RUN = "http://127.0.0.1:" + RUN_PORT
    try:
        time.sleep(3)
        def post(path, body):
            req = urllib.request.Request(RUN + path, data=json.dumps(body).encode(), method="POST",
                                         headers={"Content-Type": "application/json"})
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    return r.status, json.loads(r.read().decode() or "{}")
            except urllib.error.HTTPError as e:
                return e.code, e.read().decode()
        post("/api/study-designer/project", {"path": repo})
        # The study stored the same board as the role but a different revision.
        study = {"name": "board-line", "rows": [], "requires": {
            "dev_bench_version": "any", "firmware_version": "any",
            "build": {"board": "plank", "variant": "ns", "revision": "1"}}}

        def run_and_wait():
            code, resp = post("/api/study-designer/run", study)
            if code != 202:
                return code, resp, []
            events = []
            req = urllib.request.Request(RUN + "/api/build/events?id=" + resp["build_id"])
            with urllib.request.urlopen(req, timeout=60) as r:
                for raw in r:
                    raw = raw.decode().strip()
                    if raw.startswith("data:"):
                        ev = json.loads(raw[5:])
                        events.append(ev)
                        if ev.get("kind") in ("started", "failed"): break
            return code, resp, events

        code, resp, events = run_and_wait()
        check("a build run is accepted", code == 202, (code, resp))
        live = [e.get("text", "") for e in events if e.get("kind") == "line"]
        check("the live stream carries the board line",
              any(t.startswith("board choice:") for t in live), live[:4])
        time.sleep(1)

        rq("POST", b + "/url", {"url": RUN + "/"})
        time.sleep(2)
        script("document.querySelector('.nav-item[data-tab=\"debug\"]').click();")
        time.sleep(1)
        script("document.querySelector('.chip[data-log-source=\"builds\"]').click();")
        time.sleep(3)
        shown = script("return document.getElementById('log-console').textContent;")
        check("the builds source shows the board line after a run",
              "board choice: the study stored board 'plank', variant 'ns', revision '1'" in shown, shown[-400:])
        check("the line says what the role replaced",
              "replaces the study's revision '1'" in shown, shown[-400:])
        check("the stored log keeps the flash and reset lines",
              "flashing " in shown and "reset done" in shown, shown[-400:])

        # A run whose flash fails has the failure in the stored log.
        CORE["flash_fails"] = True
        code, resp, events = run_and_wait()
        time.sleep(1)
        logs = json.loads(urllib.request.urlopen(RUN + "/api/build/logs", timeout=30).read().decode())
        entries = logs.get("logs", logs) if isinstance(logs, dict) else logs
        newest = entries[0]["id"]
        text = urllib.request.urlopen(RUN + "/api/build/logs/" + newest, timeout=30).read().decode()
        check("a failed flash is in the stored log", "flash failed:" in text, text[-300:])
        check("the file still carries the build's own verdict", newest.endswith("-ok.log"), newest)
    finally:
        run_ui.terminate()
        srv.shutdown()

    errs = script("return window.__errs;")
    check("no uncaught JavaScript error", not errs, errs)
finally:
    rq("DELETE", b, None)
print(("\n%d FAILED" % len(fails)) if fails else "\nall passed")
sys.exit(1 if fails else 0)
