"""A Slack reply from the Desk goes to the message it was drafted for.

The Slack bar names a message by its place in the list, and a view pushed while
the bar holds focus replaces the list without redrawing it. This drives that
race in a real browser against a *stub desk* (a few lines of Python on a
scratch port): it serves a view the script controls and records every
/slack/draft and /slack/reply it receives, so nothing reaches Slack, a
connector or `claude -p`. A scratch config dir keeps the real desk's token out.

    cargo build
    geckodriver --port <gecko> &
    EMBARCH_GECKO_PORT=<gecko> EMBARCH_UI_PORT=<ui> DESK_PORT=<desk>
    python3 tests/browser/drive_desk_slack.py
"""
import json, os, shutil, subprocess, sys, tempfile, threading, time, urllib.request, urllib.error
import addr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
UI_PORT = addr.ui_port(4897)
DESK_PORT = os.environ.get("DESK_PORT", "4997")
UI = "http://127.0.0.1:" + UI_PORT
BASE = addr.gecko_url()
PUSH_WAIT = 13  # the server pushes a view every 10 s

class SetupFail(Exception):
    pass


fails = []
items = []
calls = []


def msg(ch, ts, unread=True, acct="acme"):
    return {"account": acct, "channel": ch, "channel_name": ch.lower(), "ts": ts, "user_name": "u" + ch, "kind": "mention",
            "text": "text " + ch, "permalink": "https://example.invalid/" + ch, "unread": unread, "thread_ts": None}


def view():
    n = len(items)
    return {"slack": {"items": items, "errors": [], "mentions": n, "dms": 0, "unread": sum(1 for m in items if m["unread"])},
            "connectors": [{"kind": "slack", "name": "acme"}], "hand": [], "cards": [], "drafts": [], "sessions": []}


class Stub(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, obj, code=200):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path.startswith("/view"):
            self._send(view())
        elif self.path.startswith("/health"):
            self._send({"ok": True})
        else:
            self._send({})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length") or 0)) or b"{}")
        calls.append((self.path, body))
        if self.path.startswith("/slack/draft"):
            self._send({"draft": "draft for " + body["channel"], "thread_ts": body["ts"]})
        else:
            self._send({})


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


def wd(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]


def wait_for(url, secs=20):
    for _ in range(secs * 10):
        try:
            urllib.request.urlopen(url, timeout=1)
            return True
        except Exception:
            time.sleep(0.1)
    return False


def main():
    scratch = Path(tempfile.mkdtemp(prefix="drive-desk-slack-"))
    (scratch / "cfg").mkdir()
    (scratch / "cfg" / "desk-token").write_text("scratch-token\n")
    env = {**os.environ, "EMBARCH_DESK_CONFIG_DIR": str(scratch / "cfg"), "EMBARCH_DESK_PORT": DESK_PORT,
           "EMBARCH_UI_CONFIG": str(scratch / "ui.toml"), "EMBARCH_UI_PORT": UI_PORT, "CLAUDE_CONFIG_DIR": str(scratch / "claude")}
    (scratch / "ui.toml").write_text('[core]\nbase_url = "http://127.0.0.1:1"\ntoken = "x"\n')
    srv = ThreadingHTTPServer(("127.0.0.1", int(DESK_PORT)), Stub)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    ui = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        check("the scratch UI starts", wait_for(UI + "/"))
        sid = wd("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
        S = BASE + "/session/" + sid
        try:
            def js(src, *args):
                return wd("POST", S + "/execute/sync", {"script": src, "args": list(args)})

            def click(css):
                el = wd("POST", S + "/element", {"using": "css selector", "value": css})
                wd("POST", S + "/element/" + list(el.values())[0] + "/click", {})

            def poll(src):
                """js(), but a script error (the page mid-reload) reads as false."""
                try:
                    return js(src)
                except urllib.error.HTTPError:
                    return False

            def until(src, secs=15):
                for _ in range(secs * 5):
                    if poll(src):
                        return True
                    time.sleep(0.2)
                return False

            def fresh(initial):
                """A new page on `initial` messages, the Slack bar open, no reply box."""
                items[:] = initial
                calls.clear()
                wd("POST", S + "/url", {"url": UI + "/?r=%d#desk" % time.time()})
                if not until("var c = document.getElementById('desk-slack-counts'); return c && /mention/.test(c.textContent)"):
                    raise SetupFail("the Slack mention count never appeared")
                # the bar is open, or the toggle is laid out and a click on it lands
                opened = False
                for _ in range(75):
                    if poll("return document.getElementById('desk-slack').hidden === false"):
                        opened = True
                        break
                    if poll("var t = document.getElementById('desk-slack-toggle');"
                          " return !!t && t.offsetParent !== null && t.getBoundingClientRect().width > 0"):
                        try:
                            click("#desk-slack-toggle")
                        except urllib.error.HTTPError:
                            pass  # not interactable yet; poll again
                    time.sleep(0.2)
                if not opened and not poll("return document.getElementById('desk-slack').hidden === false"):
                    raise SetupFail("the Slack toggle never became clickable / the bar never opened")
                if not until("return document.querySelectorAll('[data-draft]').length > 0"):
                    raise SetupFail("no [data-draft] button appeared in the open bar: " + str(js(
                        "var b = document.getElementById('desk-slack'); return [b.hidden, b.innerHTML.slice(0, 300),"
                        " document.getElementById('desk-slack-counts').textContent]")))

            def draft_on(channel):
                click('[data-draft][data-msg*=\'"%s"\']' % channel)
                until("return !!document.querySelector('[data-send]')")

            def hold(channel):
                click('[data-reply-box] textarea')  # focus inside the bar: pushed views are held back

            def pushed(new):
                items[:] = new
                time.sleep(PUSH_WAIT)

            def replies():
                return [b for p, b in calls if p.startswith("/slack/reply")]

            def drafts():
                return [b for p, b in calls if p.startswith("/slack/draft")]

            X, Y, Z, N = msg("CX", "100.1"), msg("CY", "90.1"), msg("CZ", "80.1"), msg("CN", "200.1")

            # 1. A new unread message lands at the top between Draft and Send.
            fresh([X, Y, Z])
            draft_on("CY")
            hold("CY")
            pushed([N, X, Y, Z])
            click("[data-send]")
            time.sleep(0.5)
            r = replies()
            check("a new message at the top does not redirect the reply", len(r) == 1 and r[0]["channel"] == "CY" and r[0]["thread_ts"] == "90.1", r)

            # 2. The message changes to read (and the list reorders) in between.
            fresh([X, Y, Z])
            draft_on("CZ")
            hold("CZ")
            pushed([msg("CZ", "80.1", unread=False), X, Y][::-1])
            click("[data-send]")
            time.sleep(0.5)
            r = replies()
            check("a message that turned read still gets its own reply", len(r) == 1 and r[0]["channel"] == "CZ" and r[0]["thread_ts"] == "80.1", r)

            # 3. A second Draft click on a card the held view has already shifted.
            fresh([X, Y, Z])
            draft_on("CX")
            hold("CX")
            pushed([N, X, Y, Z])
            click('[data-draft][data-msg*=\'"CY"\']')
            time.sleep(1)
            d = drafts()
            check("Draft on a shifted card drafts the message shown", len(d) == 2 and d[1]["channel"] == "CY" and d[1]["ts"] == "90.1", d)

            # 4. The message leaves the list: the box says so, Send posts nothing.
            fresh([X, Y, Z])
            draft_on("CY")
            hold("CY")
            pushed([X, Z])
            says = js("var b = document.querySelector('[data-reply-box]:not([hidden])'); return b && b.textContent")
            check("a reply box whose message left the list says so", says and "left the list" in says, says)
            check("its Send button is disabled", js("return document.querySelector('[data-send]').disabled"))
            js("document.querySelector('[data-send]').disabled = false; return true")  # even forced, nothing is sent
            click("[data-send]")
            time.sleep(0.5)
            check("and nothing is sent", replies() == [], replies())
            check("the typed draft is still there to copy", js("return document.querySelector('[data-reply-box] textarea').value") == "draft for CY")
        except SetupFail as e:
            check("scenario setup: " + str(e), False)
        finally:
            wd("DELETE", S)
    finally:
        ui.terminate()
        srv.shutdown()
        shutil.rmtree(scratch, ignore_errors=True)
    print("\n%d failed" % len(fails) if fails else "\nall passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
