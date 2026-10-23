"""The Desk tab, in a real browser, against a scratch embarch-desk.

Starts its own embarch-desk on a scratch port with scratch config and data
directories (so the real desk, its tokens and its state are never touched),
seeds it through the desk's own HTTP surface — a standalone work item, a
session that a hook puts on it, a commit-less line, a flash — then starts an
embarch-ui pointed at that desk and a dead Core, and drives the tab: the card
and its tile render, ✓ on a tile nobody has opened opens it instead of
sending, the dialog shows the lines, ✕ discards, the Dashboard is gone.

No connector is configured, so nothing reaches Linear, Harvest or Slack;
GitHub falls back to the machine's `gh` login for a read-only identity check.

    cargo build && (cd ../embarch-desk && cargo build)
    geckodriver --port 4444 &
    python3 tests/browser/drive_desk.py
"""
import base64, json, os, shutil, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
DESK_BIN = os.environ.get("EMBARCH_DESK_BIN", str(REPO.parent / "embarch-desk" / "target" / "debug" / "embarch-desk"))
UI_PORT = os.environ.get("DESK_UI_PORT", "4896")
DESK_PORT = os.environ.get("DESK_PORT", "4998")
UI = "http://127.0.0.1:" + UI_PORT
DESK = "http://127.0.0.1:" + DESK_PORT
BASE = "http://127.0.0.1:4444"
SHOT = os.environ.get("DESK_SHOT")

fails = []


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


def desk(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(DESK + path, data=data, method=method,
                                 headers={"Content-Type": "application/json", "Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())


def wait_for(url, secs=20):
    for _ in range(secs * 10):
        try:
            urllib.request.urlopen(url, timeout=1)
            return True
        except Exception:
            time.sleep(0.1)
    return False


def main():
    scratch = Path(tempfile.mkdtemp(prefix="drive-desk-"))
    env = {**os.environ, "EMBARCH_DESK_CONFIG_DIR": str(scratch / "cfg"), "EMBARCH_DESK_DATA_DIR": str(scratch / "data"),
           "EMBARCH_DESK_PORT": DESK_PORT}
    cfg = scratch / "ui.toml"
    cfg.write_text('[core]\nbase_url = "http://127.0.0.1:1"\ntoken = "x"\n')
    procs = []
    try:
        procs.append(subprocess.Popen([DESK_BIN, "serve"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        check("the scratch desk starts", wait_for(DESK + "/health"))
        token = (scratch / "cfg" / "desk-token").read_text().strip()

        repo = scratch / "repo"
        repo.mkdir()
        item = desk("POST", "/items", {"title": "Client sync call"}, token)["id"]
        hook = {"hook_event_name": "SessionStart", "session_id": "s1", "cwd": str(repo)}
        out = subprocess.run([DESK_BIN, "hook"], input=json.dumps(hook), env=env, capture_output=True, text=True).stdout
        check("a new session is asked which work item it is", "desk_set_work_item" in out, out[:120])
        desk("POST", "/sessions/s1/assign", {"item_id": item}, token)
        desk("POST", "/sessions/s1/line", {"text": "Agreed the bootload test plan"}, token)
        flash = {"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo),
                 "tool_name": "mcp__embarch-api__flash", "tool_input": {}, "tool_response": {}}
        subprocess.run([DESK_BIN, "hook"], input=json.dumps(flash), env=env, capture_output=True, text=True)
        today = desk("GET", "/view", token=token)["today"]
        desk("PATCH", "/tiles/%s@%s" % (item, today), {"hours": 1.5}, token)

        procs.append(subprocess.Popen([BIN], env={**env, "EMBARCH_UI_CONFIG": str(cfg), "EMBARCH_UI_PORT": UI_PORT},
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        check("the scratch UI starts", wait_for(UI + "/"))

        sid = wd("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
        S = BASE + "/session/" + sid
        try:
            def js(src, *args):
                return wd("POST", S + "/execute/sync", {"script": src, "args": list(args)})

            def settle():
                time.sleep(0.6)

            wd("POST", S + "/window/rect", {"width": 1400, "height": 950})
            wd("POST", S + "/url", {"url": UI + "/#desk"})
            for _ in range(50):
                if js("return document.querySelectorAll('.desk-card').length") > 0:
                    break
                time.sleep(0.2)
            check("the nav says Desk and nothing says Dashboard",
                  js("return document.querySelector('.nav-item[data-tab=desk] .nav-label').textContent") == "Desk"
                  and js("return !document.querySelector('[data-tab=dashboard]') && !document.getElementById('dashboard-alerts-list')"))
            check("the Desk tab is the one showing", js("return document.querySelector('.tab-panel[data-tab=desk]').classList.contains('active')"))
            check("an old #dashboard link lands on the Desk",
                  js("location.hash = '#dashboard'; return true") and (settle() or True)
                  and js("return document.querySelector('.tab-panel[data-tab=desk]').classList.contains('active')"))
            check("the card renders with its title", js("return document.querySelector('.desk-card-title').textContent") == "Client sync call")
            check("its bench activity shows", "flash ×1" in js("return document.querySelector('.desk-card-meta').textContent"))
            tile = js("var t = document.querySelector('.desk-tile'); return t && {hours: t.querySelector('.desk-tile-hours').textContent, ok: !!t.querySelector('.desk-ok'), no: !!t.querySelector('.desk-no')}")
            check("today's tile is a square with its hours and ✓ ✕", tile and tile["hours"] == "1h30" and tile["ok"] and tile["no"], tile)
            if SHOT:
                Path(SHOT).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))

            js("document.querySelector('.desk-ok').click(); return true")
            settle()
            check("✓ on a tile nobody opened opens it instead of sending",
                  js("return !document.getElementById('desk-dialog').hidden"))
            check("the dialog shows the session's line",
                  "Agreed the bootload test plan" in js("return document.querySelector('.desk-lines').textContent"))
            if SHOT:
                Path(SHOT.replace(".png", "-dialog.png")).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))
            js("document.getElementById('desk-approve-big').click(); return true")
            settle()
            check("sending with no Harvest project says what is missing",
                  "Harvest project" in js("return document.getElementById('desk-dialog-status').textContent"),
                  js("return document.getElementById('desk-dialog-status').textContent"))
            js("document.getElementById('desk-dialog-close').click(); return true")

            js("window.confirm = function () { return true; }; document.querySelector('.desk-no').click(); return true")
            settle()
            check("✕ discards the tile and it leaves the card", js("return document.querySelectorAll('.desk-tile').length") == 0)

            desk("POST", "/items", {"title": "Pushed card"}, token)
            pushed = False
            for _ in range(70):
                if "Pushed card" in js("return document.getElementById('desk-cards').textContent"):
                    pushed = True
                    break
                time.sleep(0.2)
            check("a card made elsewhere arrives by SSE, no click and no client polling", pushed)
            check("desk.js holds no interval", "setInterval" not in (REPO / "assets" / "desk.js").read_text())

            # An agent's write is a draft on the tab, never sent on its own.
            desk("POST", "/drafts", {"kind": "linear_comment", "issue_key": "ABC-1", "body": "Flashed and passed."}, token)
            shown = False
            for _ in range(70):
                if "Comment on ABC-1" in js("return document.getElementById('desk-drafts').textContent"):
                    shown = True
                    break
                time.sleep(0.2)
            check("an agent's comment shows as a draft with Send and Discard", shown and
                  js("return !!document.querySelector('[data-draft-send]') && !!document.querySelector('[data-draft-discard]')"))
            js("document.querySelector('[data-draft-discard]').click(); return true")
            settle()
            check("Discard drops the draft and nothing is left to send", js("return document.getElementById('desk-drafts').hidden"))

            js("Array.from(document.querySelectorAll('.desk-card')).find(function (c) {"
               " return c.querySelector('.desk-card-title').textContent === 'Client sync call';"
               "}).querySelector('[data-timer-start]').click(); return true")
            settle()
            check("Start timer shows a running timer on the card's item",
                  "Client sync call" in js("return document.getElementById('desk-timer').textContent")
                  and js("return !!document.querySelector('.desk-card [data-timer-stop]')"))
            js("document.querySelector('#desk-timer [data-timer-stop]').click(); return true")
            settle()
            check("Stop ends it", js("return document.getElementById('desk-timer').hidden"))

            # --- desks: one per project, one open suite-wide -------------------
            (repo / "CMakeLists.txt").write_text("project(fw)\n")
            js("document.getElementById('desk-create').click(); return true")
            for _ in range(150):  # the form waits on Linear, Harvest and GitHub
                if js("return !!document.getElementById('df-name')"):
                    break
                time.sleep(0.2)
            js("document.getElementById('df-name').value = 'Bench client';"
               "document.getElementById('df-repo').value = arguments[0];"
               "document.querySelector('[data-form-save]').click(); return true", str(repo))
            opened = False
            for _ in range(60):
                if js("return document.getElementById('project-current-name').textContent") == "Bench client":
                    opened = True
                    break
                time.sleep(0.2)
            check("a desk made from the form opens, and the sidebar names it", opened,
                  js("return document.getElementById('project-current-name').textContent"))
            check("the Desk tab names the open desk", js("return document.getElementById('desk-head-name').textContent") == "Bench client")
            check("the first desk adopts the cards that came before it",
                  "Client sync call" in js("return document.getElementById('desk-cards').textContent"))
            day = js("return document.getElementById('desk-day-body').textContent")
            check("Today's work groups by desk and shows what was done",
                  "Bench client" in day and "Client sync call" in day and "Agreed the bootload test plan" in day, day[:200])
            js("document.getElementById('desk-new').value = 'Weekly call';"
               "document.getElementById('desk-add').click(); return true")
            settle()
            check("a standalone card lands on the desk picked for it (the open one by default)",
                  "Weekly call" in js("return document.getElementById('desk-cards').textContent"))

            # A switch made elsewhere (an agent, the CLI) reaches the page by itself.
            desk("POST", "/open", {"desk": "none"}, token)
            followed = False
            for _ in range(70):
                if js("return document.getElementById('project-current-path').textContent").startswith("no desk"):
                    followed = True
                    break
                time.sleep(0.2)
            check("closing the desk from outside the page reaches the sidebar", followed,
                  js("return document.getElementById('project-current-path').textContent"))
            ks = desk("GET", "/desks", token=token)["desks"]
            k = [d for d in ks if d["name"] == "Bench client"][0]
            desk("POST", "/desks", {"id": k["id"], "name": "Bench client", "firmware_repos": [str(repo)], "bench": "two-boards"}, token)
            desk("POST", "/open", {"desk": "Bench client"}, token)
            banner = False
            for _ in range(70):
                if "two-boards" in js("return document.getElementById('desk-bench-banner').textContent"):
                    banner = True
                    break
                time.sleep(0.2)
            check("opening a desk proposes its saved bench and touches nothing", banner and
                  js("return !!document.getElementById('desk-bench-review')"))
            js("document.getElementById('desk-bench-later').click(); return true")

            js("document.getElementById('desk-settings-toggle').click(); return true")
            settle()
            check("the Connectors panel opens with its add form", js("return !!document.getElementById('desk-conn-token')"))
            js("document.getElementById('desk-slack-toggle').click(); return true")
            settle()
            check("the Slack bar expands to its feed and says nothing is connected",
                  "No Slack workspace connected" in js("return document.getElementById('desk-slack').textContent"))
        finally:
            wd("DELETE", S)

        # The real desk is down: the proxy says so in words, not a stack trace.
        procs[0].terminate()
        procs[0].wait(5)
        try:
            urllib.request.urlopen(UI + "/api/desk/view", timeout=5)
            check("a stopped desk is reported as stopped", False)
        except urllib.error.HTTPError as e:
            check("a stopped desk is reported as stopped", e.code == 503 and b"not running" in e.read())
    finally:
        for p in procs:
            if p.poll() is None:
                p.terminate()
        shutil.rmtree(scratch, ignore_errors=True)

    print("\n%d failed" % len(fails) if fails else "\nall passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
