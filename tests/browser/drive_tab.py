"""A tab folder, in a real browser: the extension point embarch-ui's config
names (`src/tabs.rs`).

Starts its own embarch-ui against a dead Core, and a stand-in backend that
echoes what it was sent. It copies tests/fixtures/sample-tab to a scratch folder,
fills in the backend's address and token file, and then checks: the tab sits
beside Desk and opens from a fragment and from a click; its files, panel, style
and script arrive; its calls and its event stream reach the backend with the
token added and the token stays out of the page; a call from another origin is
refused before it reaches the backend; a broken manifest leaves the UI up with
no tab and says why in the log; and with no tab configured, a browser that
remembers the tab lands on Desk rather than on a blank page.

    cargo build
    geckodriver --port 4444 &
    python3 tests/browser/drive_tab.py
"""
import http.server, json, os, shutil, subprocess, sys, tempfile, threading, time, urllib.request
import addr
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
UI_PORT = addr.ui_port(4889)
UI = "http://127.0.0.1:" + UI_PORT
BASE = addr.gecko_url()
TOKEN = "drive-tab-token"

fails = []
calls = []


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


class Echo(http.server.BaseHTTPRequestHandler):
    def answer(self):
        size = int(self.headers.get("Content-Length") or 0)
        seen = {"method": self.command, "path": self.path, "auth": self.headers.get("Authorization"),
                "body": self.rfile.read(size).decode()}
        calls.append(seen)
        data = json.dumps(seen).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = answer

    def log_message(self, *_):
        pass


def sample_folder(scratch, backend):
    folder = scratch / "sample-tab"
    if folder.exists():
        shutil.rmtree(folder)
    shutil.copytree(REPO / "tests" / "fixtures" / "sample-tab", folder)
    token = scratch / "token"
    token.write_text(TOKEN + "\n")
    manifest = folder / "tab.toml"
    manifest.write_text(manifest.read_text().replace("http://127.0.0.1:4999", backend).replace("/path/to/token", str(token)))
    return folder


def start_ui(scratch, folder):
    """A UI with `folder` as its one tab, or with no tab. Its log goes to a file."""
    cfg = scratch / "ui.toml"
    text = '[core]\nbase_url = "http://127.0.0.1:1"\ntoken = "x"\n'
    if folder:
        text += '[[tabs]]\nid = "sample"\nfolder = "%s"\n' % folder
    cfg.write_text(text)
    log = open(scratch / "ui.log", "w")
    env = {**os.environ, "EMBARCH_UI_CONFIG": str(cfg), "EMBARCH_UI_PORT": UI_PORT, "EMBARCH_UI_HOST": "127.0.0.1"}
    proc = subprocess.Popen([BIN], env=env, stdout=log, stderr=log)
    for _ in range(200):
        try:
            urllib.request.urlopen(UI + "/", timeout=1)
            return proc
        except Exception:
            time.sleep(0.1)
    raise SystemExit("the UI did not start: " + (scratch / "ui.log").read_text()[-400:])


def main():
    scratch = Path(tempfile.mkdtemp(prefix="drive-tab-"))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Echo)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    backend = "http://127.0.0.1:%d" % server.server_address[1]
    folder = sample_folder(scratch, backend)
    ui = start_ui(scratch, folder)
    sid = None
    try:
        caps = {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}}
        sid = wd("POST", BASE + "/session", caps)["sessionId"]
        s = BASE + "/session/" + sid

        def js(script):
            return wd("POST", s + "/execute/sync", {"script": script, "args": []})

        def until(script, secs=10):
            for _ in range(secs * 10):
                got = js(script)
                if got:
                    return got
                time.sleep(0.1)
            return js(script)

        def goto(url):
            wd("POST", s + "/url", {"url": "about:blank"})
            wd("POST", s + "/url", {"url": url})

        goto(UI + "/#sample")
        check("a fragment opens the tab", until("return document.querySelector('.tab-panel.active[data-tab=sample]') !== null"))
        check("the top bar names it", js("return document.querySelector('.topbar-title').textContent") == "Sample")
        order = js("return Array.from(document.querySelectorAll('.nav-item')).map(function (e) { return e.dataset.tab; })")
        check("it sits beside Desk", order[:3] == ["desk", "sample", "topology"], order)
        check("its icon is inline, so it follows the theme",
              js("return document.querySelector('.nav-item[data-tab=sample] svg circle') !== null"))
        check("its style reached the panel",
              js("return getComputedStyle(document.querySelector('.sample-card')).outlineWidth") == "3px")
        check("its script ran", until("return document.getElementById('sample-script').textContent") == "the script ran")

        result = until("var t = document.getElementById('sample-result').textContent; return t.indexOf('{') === 0 ? t : ''")
        said = json.loads(result)
        check("a call reaches the backend", said["path"] == "/hello?x=1" and said["method"] == "POST" and said["body"] == "from the page", said)
        check("the call carries the token", said["auth"] == "Bearer " + TOKEN, said["auth"])
        pushed = until("var t = document.getElementById('sample-pushed').textContent; return t.indexOf('{') === 0 ? t : ''")
        said = json.loads(pushed)
        check("the event stream pushes the backend's path", said["path"] == "/view" and said["auth"] == "Bearer " + TOKEN, said)
        # What the server hands the browser. The live page cannot be the test: this
        # stand-in echoes the header it got, and the sample tab displays that.
        served = urllib.request.urlopen(UI + "/", timeout=5).read().decode()
        served += urllib.request.urlopen(UI + "/tab/sample/tab.toml", timeout=5).read().decode()
        check("the token stays out of what is served", "tab-panel" in served and TOKEN not in served)

        # A click from another tab, and back.
        js("document.querySelector('.nav-item[data-tab=atlas]').click()")
        check("another tab still opens", js("return document.querySelector('.tab-panel.active').dataset.tab") == "atlas")
        js("document.querySelector('.nav-item[data-tab=sample]').click()")
        check("a click opens the tab", js("return document.querySelector('.tab-panel.active').dataset.tab") == "sample")
        goto(UI + "/#desk")
        check("the Desk tab still draws", until("return document.querySelector('.tab-panel.active[data-tab=desk]') !== null"))
        js("document.querySelector('.nav-item[data-tab=sample]').click()")

        # Another origin: the same server by another name is another site to a browser.
        other = "http://localhost:" + UI_PORT
        goto(other + "/#atlas")
        before = len(calls)
        wd("POST", s + "/execute/async", {"script": """
            var done = arguments[arguments.length - 1];
            fetch(%s, { method: 'POST', mode: 'no-cors', headers: { 'Content-Type': 'text/plain' }, body: 'x' })
              .then(function () { done('sent'); }, function () { done('blocked'); });
        """ % json.dumps(UI + "/api/tab/sample/echo/stolen"), "args": []})
        time.sleep(0.5)
        check("a call from another origin never reaches the backend", not any(c["path"] == "/stolen" for c in calls[before:]))
        loaded = wd("POST", s + "/execute/async", {"script": """
            var done = arguments[arguments.length - 1];
            var el = document.createElement('script');
            el.src = %s;
            el.onload = function () { done('loaded'); };
            el.onerror = function () { done('refused'); };
            document.head.appendChild(el);
        """ % json.dumps(UI + "/tab/sample/tab.js"), "args": []})
        check("another origin cannot load the tab's script", loaded == "refused", loaded)

        # A broken manifest: the UI comes up without the tab and says why.
        ui.terminate(); ui.wait()
        bad = sample_folder(scratch, "http://10.0.0.1:4892")
        ui = start_ui(scratch, bad)
        goto(UI + "/")
        check("a broken tab is left out", js("return document.querySelector('[data-tab=sample]') === null"))
        check("the page lands on a real tab, not on the broken one's memory",
              until("var a = document.querySelector('.tab-panel.active'); return a && a.dataset.tab") == "desk")
        logged = (scratch / "ui.log").read_text()
        check("the log says why", "tab sample is left out" in logged and "loopback" in logged, logged[-200:])

        # No tab configured, with the browser still remembering it.
        ui.terminate(); ui.wait()
        ui = start_ui(scratch, None)
        goto(UI + "/")
        js("localStorage.setItem('embarch-ui.tab', 'sample')")
        goto(UI + "/")
        check("a remembered tab that is gone lands on Desk",
              until("var a = document.querySelector('.tab-panel.active'); return a && a.dataset.tab") == "desk")
        check("no tab means no sign of one",
              js("return document.querySelector('[data-tab=sample]') === null && document.documentElement.outerHTML.indexOf('/tab/') < 0"))
        js("localStorage.removeItem('embarch-ui.tab')")
    finally:
        if sid:
            wd("DELETE", BASE + "/session/" + sid)
        ui.terminate()
        server.shutdown()
        shutil.rmtree(scratch, ignore_errors=True)
    print("%d failed" % len(fails) if fails else "all passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
