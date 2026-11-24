"""The Desk tab's Promptu section (embarch-desk decision 65, ui/113), in a real
browser, against a stand-in desk.

The stand-in desk is this script's own HTTP server on a scratch port, with a
scratch token file, so no real desk or Promptu is touched. It answers /view with
an empty desk and the /promptu routes from a sheet it holds, and records every
request's method, path and body. The drive checks: with `open: 0` the section
takes no space on the tab; with lines open the bar names the week and the
count; the panel shows the numbers (a null as no data, never zero), each group
and line; a line's ✕ with its note posts the mark and note; a group's ✕ posts
a cut; the feel line posts `{"feel": ...}`; Apply posts and a line that failed
stays open with its error; and a 400 and a 503 are shown in the desk's words
with the panel still usable. `POST /promptu/feel/{week}` is the route desk/043
adds; the stand-in answers it in the shape that task fixed.

    cargo build
    geckodriver --port 4444 &
    python3 tests/browser/drive_desk_promptu.py
"""
import http.server, json, os, subprocess, tempfile, threading, time, urllib.request
import addr
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
UI_PORT = addr.ui_port(4897)
DESK_PORT = os.environ.get("DESK_PORT", "4997")
UI = "http://127.0.0.1:" + UI_PORT
BASE = addr.gecko_url()
TOKEN = "drive-desk-promptu-token"

fails = []
calls = []
state = {"open": 0, "fail": None, "sheet": None}


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


def wd(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read().decode())["value"]
    except urllib.error.HTTPError as e:
        raise RuntimeError("webdriver: " + e.read().decode()[:600]) from None


def fresh_sheet():
    def line(i, text, kind="pattern", status="open"):
        return {"id": "l%d" % i, "kind": kind, "verdict": "approve", "note": "", "text": text, "count": 1,
                "events": [], "status": status, "error": None, "commit": None}
    return {"promptu": 1, "week": "2026-W48", "feel": None,
            "numbers": [{"check": "fills_kept", "value": 0.93, "baseline": None, "target": ">= 0.90"},
                        {"check": "questions_per_run", "value": None, "baseline": 2.0, "target": "<= 2"}],
            "groups": [{"id": "g1", "kind": "pattern", "title": "implement-issue", "cut": False,
                        "lines": [line(1, "add option Builds"), line(2, "reword the done question")]},
                       {"id": "g2", "kind": "memory", "title": "memory", "cut": False,
                        "lines": [line(3, "write the bench note", "memory")]}]}


def lines_of(sheet):
    return {l["id"]: l for g in sheet["groups"] for l in g["lines"]}


class Desk(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, status, obj):
        data = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def handle_any(self):
        size = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(size).decode()
        body = json.loads(raw) if raw else None
        path = self.path.split("?")[0]
        sheet = state["sheet"]
        if path == "/view":
            return self.send(200, {"today": "2026-11-25", "hand": [], "cards": [], "desks": [], "open_desk": None,
                                   "settings": None, "connectors": [], "drafts": [], "ignore": [], "sessions_waiting": 0,
                                   "day": {"date": "2026-11-25", "items": [], "total_hours": 0}, "desk_notes": None})
        if not path.startswith("/promptu"):
            return self.send(404, {"error": "not here"})
        calls.append({"method": self.command, "path": path, "body": body})
        if state.get("down"):
            return self.send(503, {"error": "promptu is not installed (no `promptu` binary found)"})
        if path == "/promptu":
            return self.send(200, {"promptu": 1, "open": state["open"], "week": "2026-W48"} if state["open"] else {"promptu": 1, "open": 0})
        if path == "/promptu/sheet":
            return self.send(200, sheet)
        parts = path.split("/")
        if parts[2] == "sheet" and len(parts) == 5:
            if state.get("refuse"):
                return self.send(400, {"error": state["refuse"]})
            cut = body["verdict"] == "cut"
            for g in sheet["groups"]:
                if g["id"] == parts[4]:
                    g["cut"] = cut
                    for l in g["lines"]:
                        l["status"] = "cut" if cut else "open"
                for l in g["lines"]:
                    if l["id"] == parts[4]:
                        l["verdict"] = body["verdict"]
                        l["status"] = "cut" if cut else "open"
                        l["note"] = body.get("note", "")
            return self.send(200, {"promptu": 1, "week": sheet["week"], "id": parts[4], "verdict": body["verdict"]})
        if parts[2] == "feel":
            sheet["feel"] = body["feel"]
            return self.send(200, {"promptu": 1, "week": sheet["week"], "feel": body["feel"]})
        if parts[2] == "apply":
            out = []
            for l in lines_of(sheet).values():
                if l["status"] != "open":
                    continue
                if l["id"] == state["fail"]:
                    l["status"] = "failed"
                    l["error"] = "the memory check refused the topic file"
                else:
                    l["status"] = "applied"
                out.append({"id": l["id"], "status": l["status"], "commit": None, "error": l["error"]})
            state["open"] = sum(1 for l in lines_of(sheet).values() if l["status"] in ("open", "failed"))
            return self.send(200, {"promptu": 1, "week": sheet["week"], "applied": 2, "failed": 1, "lines": out})
        self.send(404, {"error": "no such route"})

    do_GET = do_POST = do_PATCH = do_DELETE = handle_any


def main():
    scratch = Path(tempfile.mkdtemp(prefix="drive-desk-promptu-"))
    (scratch / "cfg" / "embarch-desk").mkdir(parents=True)
    (scratch / "cfg" / "embarch-desk" / "desk-token").write_text(TOKEN)
    cfg = scratch / "ui.toml"
    cfg.write_text('[core]\nbase_url = "http://127.0.0.1:1"\nperson_token = "x"\n')
    env = {**os.environ, "XDG_CONFIG_HOME": str(scratch / "cfg"), "EMBARCH_DESK_CONFIG_DIR": str(scratch / "cfg" / "embarch-desk"),
           "EMBARCH_DESK_PORT": DESK_PORT, "EMBARCH_UI_CONFIG": str(cfg), "EMBARCH_UI_PORT": UI_PORT}
    state["sheet"] = fresh_sheet()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", int(DESK_PORT)), Desk)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    ui = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(200):
            try:
                urllib.request.urlopen(UI + "/", timeout=1)
                break
            except Exception:
                time.sleep(0.1)
        sid = wd("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
        S = BASE + "/session/" + sid
        try:
            def js(src, *args):
                return wd("POST", S + "/execute/sync", {"script": src, "args": list(args)})

            def until(src, secs=15):
                v = None
                for _ in range(secs * 5):
                    v = js(src)
                    if v:
                        return v
                    time.sleep(0.2)
                return v

            def el(css):
                return list(wd("POST", S + "/element", {"using": "css selector", "value": css}).values())[0]

            def click(css):
                wd("POST", S + "/element/" + el(css) + "/click", {})

            def type_into(css, text):
                wd("POST", S + "/element/" + el(css) + "/value", {"text": text})

            def last(method, path_prefix):
                m = [c for c in calls if c["method"] == method and c["path"].startswith(path_prefix)]
                return m[-1] if m else None

            wd("POST", S + "/window/rect", {"width": 1400, "height": 950})
            wd("POST", S + "/url", {"url": UI + "/#desk"})
            until("return !!document.getElementById('desk-promptu') && document.querySelector('.desk-head-name') && "
                  "document.querySelector('.desk-head-name').textContent !== ''")
            until("return " + str(len(calls)) + " >= 0 && true")
            time.sleep(1.5)
            gone = js("var b = document.getElementById('desk-promptu'); var r = b.getBoundingClientRect();"
                      " return {hidden: b.hidden, w: r.width, h: r.height, shown: getComputedStyle(b).display}")
            check("with open: 0 the section takes no space on the tab",
                  gone["hidden"] and gone["w"] == 0 and gone["h"] == 0 and gone["shown"] == "none", gone)
            check("the bar asked the desk for /promptu", last("GET", "/promptu") is not None)

            # Lines open: the bar names the week and the count.
            state["open"] = 3
            js("document.querySelector('.nav-item[data-tab=desk]').click(); return true")
            label = until("var b = document.getElementById('desk-promptu'); return !b.hidden && b.textContent")
            check("with lines open the bar names the week and how many wait", label == "Promptu · 2026-W48 · 3 waiting", label)

            click("#desk-promptu")
            until("return document.querySelectorAll('#desk-promptu-panel .pr-line').length === 3")
            panel = js("var p = document.getElementById('desk-promptu-panel'); return {"
                       "rows: Array.from(p.querySelectorAll('.pr-numbers tbody tr')).map(function (r) { return Array.from(r.children).map(function (c) { return c.textContent; }); }),"
                       "groups: Array.from(p.querySelectorAll('.pr-group-head')).map(function (g) { return g.textContent; }),"
                       "lines: p.querySelectorAll('.pr-line').length, before: !!(p.querySelector('.pr-numbers').compareDocumentPosition(p.querySelector('.pr-group')) & 4),"
                       "feel: Array.from(p.querySelectorAll('[data-pr-feel]')).map(function (b) { return b.textContent; })}")
            check("the numbers come first, a null shown as no data and never zero",
                  panel["before"] and panel["rows"][0][1] == "0.93" and panel["rows"][0][2] == "no data"
                  and panel["rows"][1][1] == "no data" and not any(c == "0" for r in panel["rows"] for c in r), panel["rows"])
            check("each group shows its title and line count, each line its own row",
                  "implement-issue" in panel["groups"][0] and "2 lines" in panel["groups"][0] and "1 line" in panel["groups"][1]
                  and panel["lines"] == 3, panel["groups"])
            check("the feel line offers better, same and worse", panel["feel"] == ["better", "same", "worse"], panel["feel"])
            check("Apply counts the approved open lines", js("return document.getElementById('desk-promptu-apply').textContent") == "Apply ✓ (3)")

            # A line's ✕ with a note.
            type_into('[data-pr-note="l2"]', "too wordy")
            click('.pr-line[data-line="l2"] [data-pr-mark="cut"]')
            until("return document.querySelector('.pr-line[data-line=l2]').classList.contains('cut')")
            m = last("POST", "/promptu/sheet/2026-W48/l2")
            check("✕ on a line posts its verdict and the note to /promptu/sheet/{week}/{id}",
                  m and m["body"] == {"verdict": "cut", "note": "too wordy"}, m)
            check("the cut line shows cut and Apply counts one fewer",
                  js("return document.getElementById('desk-promptu-apply').textContent") == "Apply ✓ (2)")
            click('.pr-line[data-line="l2"] [data-pr-mark="approve"]')
            until("return !document.querySelector('.pr-line[data-line=l2]').classList.contains('cut')")
            m = last("POST", "/promptu/sheet/2026-W48/l2")
            check("✓ on it posts approve", m and m["body"]["verdict"] == "approve", m)

            # A group's ✕.
            click('.pr-group[data-group="g2"] [data-pr-mark="cut"]')
            until("return document.querySelector('.pr-line[data-line=l3]').classList.contains('cut')")
            m = last("POST", "/promptu/sheet/2026-W48/g2")
            check("✕ on a group posts a cut for the group id, no note",
                  m and m["body"] == {"verdict": "cut"}, m)

            # The feel line.
            click('[data-pr-feel="worse"]')
            until("return document.querySelector('[data-pr-feel=worse]').classList.contains('on')")
            m = last("POST", "/promptu/feel/2026-W48")
            check("a feel button posts {feel} to /promptu/feel/{week} and shows the choice", m and m["body"] == {"feel": "worse"}, m)

            # Apply: l1 fails, l3 stays cut.
            click('.pr-group[data-group="g2"] [data-pr-mark="approve"]')
            until("return !document.querySelector('.pr-line[data-line=l3]').classList.contains('cut')")
            state["fail"] = "l3"
            click("#desk-promptu-apply")
            until("return !!document.getElementById('desk-promptu-result')")
            m = last("POST", "/promptu/apply/2026-W48")
            check("Apply posts to /promptu/apply/{week}", m is not None and m["body"] is None, m)
            res = js("var r = document.getElementById('desk-promptu-result'); return r.textContent")
            check("each line's result is shown", "l1 · applied" in res and "l3 · failed" in res, res)
            failed = js("var l = document.querySelector('.pr-line[data-line=l3]');"
                        " return {status: l.querySelector('.pr-status').textContent, err: l.querySelector('.pr-err') && l.querySelector('.pr-err').textContent}")
            check("a line that failed stays open on the sheet with its error shown",
                  failed["status"] == "failed" and failed["err"] == "the memory check refused the topic file", failed)
            check("the bar still counts the failed line", until("return document.getElementById('desk-promptu').textContent.indexOf('1 waiting') >= 0"))

            # Errors in the desk's words, the panel still usable.
            state["refuse"] = "Promptu: l3 is settled"
            click('.pr-line[data-line="l3"] [data-pr-mark="cut"]')
            err = until("var e = document.getElementById('desk-promptu-error'); return e && e.textContent")
            check("a 400 shows Promptu's own words and the sheet stays", err == "Promptu: l3 is settled"
                  and js("return document.querySelectorAll('#desk-promptu-panel .pr-line').length") == 3, err)
            state["refuse"] = None
            state["down"] = True
            click("#desk-promptu-apply")
            err = until("var e = document.getElementById('desk-promptu-error'); return e && e.textContent.indexOf('not installed') >= 0 && e.textContent")
            check("a 503 names the missing binary", bool(err) and "promptu" in err, err)
            check("and the panel is still usable", js("return !document.getElementById('desk-promptu-apply').disabled"))
            state["down"] = False
        finally:
            try:
                wd("DELETE", S)
            except Exception:
                pass
    finally:
        ui.terminate()
        server.shutdown()
    print("\n%d failed" % len(fails) if fails else "\nall passed")
    raise SystemExit(1 if fails else 0)


if __name__ == "__main__":
    main()
