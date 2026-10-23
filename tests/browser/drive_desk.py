"""The Desk tab, in a real browser, against a scratch embarch-desk.

Starts its own embarch-desk on a scratch port with scratch config and data
directories (so the real desk, its tokens and its state are never touched),
seeds it through the desk's own HTTP surface — a standalone work item, a
session that a hook puts on it, a commit-less line, a flash — then starts an
embarch-ui pointed at that desk and a dead Core, and drives the tab: the card
and its tile render, ✓ on a tile nobody has opened opens it instead of
sending, the dialog shows the lines, ✕ discards, the Dashboard is gone. The
chat's part (embarch-chat-desk-proposal.md): a session as one line, Open chat
through the mailbox, the card's notes from memory files written in a scratch
Claude folder, the chat setting, Start work with and without a prompt link.

No connector is configured, so nothing reaches Linear, Harvest or Slack;
GitHub falls back to the machine's `gh` login for a read-only identity check.
Start work needs Linear, so its answer is stubbed in the page.

    cargo build && (cd ../embarch-desk && cargo build)
    geckodriver --port 4444 &
    python3 tests/browser/drive_desk.py
"""
import base64, json, os, shutil, subprocess, sys, tempfile, time, urllib.parse, urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
DESK_BIN = os.environ.get("EMBARCH_DESK_BIN", str(REPO.parent / "embarch-desk" / "target" / "debug" / "embarch-desk"))
UI_PORT = os.environ.get("DESK_UI_PORT", "4896")
DESK_PORT = os.environ.get("DESK_PORT", "4998")
UI = "http://127.0.0.1:" + UI_PORT
DESK = "http://127.0.0.1:" + DESK_PORT
BASE = os.environ.get("WEBDRIVER", "http://127.0.0.1:4444")
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
    # A scratch Claude folder too: the desk reads memory files from it, and
    # never imports the machine's own.
    env = {**os.environ, "EMBARCH_DESK_CONFIG_DIR": str(scratch / "cfg"), "EMBARCH_DESK_DATA_DIR": str(scratch / "data"),
           "EMBARCH_DESK_PORT": DESK_PORT, "CLAUDE_CONFIG_DIR": str(scratch / "claude")}
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
            # A session is one line — state, what it waits for, age — with
            # Jump; Answer and the last reply have left it (chat decision 32).
            for ev in ({"hook_event_name": "Stop", "session_id": "s1", "cwd": str(repo),
                        "last_assistant_message": "Summary: the loader needs 432 KB."},
                       {"hook_event_name": "Notification", "session_id": "s1", "cwd": str(repo),
                        "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}):
                subprocess.run([DESK_BIN, "hook"], input=json.dumps(ev), env=env, capture_output=True, text=True)
            row = None
            for _ in range(100):
                row = js("var r = document.querySelector('.desk-session'); return r && {text: r.textContent,"
                         " state: r.querySelector('.desk-session-state').textContent, dot: !!r.querySelector('.desk-dot'),"
                         " jump: !!r.querySelector('[data-jump-session]'), chat: !!r.querySelector('[data-open-chat]'),"
                         " links: r.querySelectorAll('a').length}")
                if row and "permission prompt" in row["text"]:
                    break
                time.sleep(0.2)
            check("a terminal session is one line: its state, what it waits for, its age, and Jump",
                  bool(row) and row["dot"] and row["state"] == "waiting" and "for permission prompt" in row["text"]
                  and ("ago" in row["text"] or "just now" in row["text"]) and row["jump"] and not row["chat"], row)
            check("Answer and the last reply have left the row", bool(row) and row["links"] == 0 and "432 KB" not in row["text"], row)

            # A chat the panel runs gets Open chat: a focus request left in
            # the desk's mailbox for its folder, then that folder's link.
            chat_dir = scratch / "repo-ABC-1"
            chat_dir.mkdir()
            desk("POST", "/sessions/c1/assign", {"item_id": item, "client": "embarch-chat"}, token)
            start = {"hook_event_name": "SessionStart", "session_id": "c1", "cwd": str(chat_dir)}
            subprocess.run([DESK_BIN, "hook"], input=json.dumps(start), env=env, capture_output=True, text=True)
            rows = None
            for _ in range(100):
                rows = js("return Array.from(document.querySelectorAll('.desk-session')).map(function (r) {"
                          " return {chat: !!r.querySelector('[data-open-chat]'), jump: !!r.querySelector('[data-jump-session]')}; })")
                if any(r["chat"] for r in rows):
                    break
                time.sleep(0.2)
            check("a chat the panel runs gets Open chat, and only the other session Jump",
                  sorted((r["chat"], r["jump"]) for r in rows) == [(False, True), (True, False)], rows)
            js("document.querySelector('[data-open-chat]').click(); return true")
            mail = []
            for _ in range(30):
                mail = desk("GET", "/mailbox?folder=" + urllib.parse.quote(str(chat_dir)), token=token)["entries"]
                if mail:
                    break
                time.sleep(0.2)
            check("Open chat leaves a focus request for the chat's folder in the desk's mailbox",
                  [(m["kind"], m["session_id"]) for m in mail] == [("focus", "c1")], mail)

            # Notes: each at its newest state on the card, every one behind a
            # click (chat decision 46). A memory file written twice keeps both
            # versions; the card shows the second.
            mem = scratch / "claude" / "projects" / "-w-fw" / "memory"
            mem.mkdir(parents=True)
            state_md = mem / "abc-1-state.md"
            for text, tool in (('---\nname: abc-1-state\ndescription: "ABC-1: branch made"\n---\n\nBranch abc-1-loader.\n', "Write"),
                               ('---\nname: abc-1-state\ndescription: "ABC-1: PR open, flash pending"\n---\n\nBranch abc-1-loader, PR open.\n', "Edit")):
                state_md.write_text(text)
                ev = {"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo), "tool_name": tool,
                      "tool_input": {"file_path": str(state_md)}, "tool_response": {}}
                subprocess.run([DESK_BIN, "hook"], input=json.dumps(ev), env=env, capture_output=True, text=True)
            desk("POST", "/sessions/s1/line", {"text": "The loader needs 432 KB of RAM", "kind": "finding"}, token)
            card = None
            for _ in range(100):
                card = js("var n = document.querySelector('.desk-notes'); return n && n.textContent")
                if card and "432 KB of RAM" in card:
                    break
                time.sleep(0.2)
            check("a card shows each note's newest state: the memory file's latest text, the latest lines with their kind",
                  bool(card) and "abc-1-state" in card and "PR open, flash pending" in card and "branch made" not in card
                  and "finding" in card and "Agreed the bootload test plan" in card, card)
            js("document.querySelector('.desk-notes').click(); return true")
            full = ""
            for _ in range(30):
                full = js("return document.getElementById('desk-notes').textContent")
                if "branch made" in full:
                    break
                time.sleep(0.2)
            check("a click opens every note, newest first, a memory file's earlier version too",
                  js("return !document.getElementById('desk-notes').hidden")
                  and full.find("432 KB of RAM") < full.find("PR open, flash pending") < full.find("branch made")
                  and "Agreed the bootload test plan" in full, full[:300])
            if SHOT:
                Path(SHOT.replace(".png", "-notes.png")).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))
            js("document.getElementById('desk-notes-close').click(); return true")
            check("Close puts the list away", js("return document.getElementById('desk-notes').hidden"))
            check("the pick-up list says why it is empty with no Linear connected",
                  "Linear" in js("return document.getElementById('desk-pickup-body').textContent"))
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
            # The chat setting sits with the desk's other settings, off by default.
            toggle = None
            for _ in range(50):
                toggle = js("var c = document.getElementById('desk-chat-panel'); return c && !c.disabled && {on: c.checked}")
                if toggle:
                    break
                time.sleep(0.2)
            check("the chat setting shows in the panel, off by default", toggle == {"on": False}, toggle)
            js("document.getElementById('desk-chat-panel').click(); return true")
            on = False
            for _ in range(30):
                if desk("GET", "/settings", token=token).get("chat_panel") is True:
                    on = True
                    break
                time.sleep(0.2)
            check("turning it on reaches the desk", on)
            js("document.getElementById('desk-chat-panel').click(); return true")
            settle()
            check("and off again", desk("GET", "/settings", token=token).get("chat_panel") is False)

            # Start work needs Linear, so the desk's answer is stubbed in the
            # page: with no Claude Code prompt link (the chat setting on) only
            # the folder opens; with one, the prompt link follows 5 s later.
            def start_with(answer):
                return wd("POST", S + "/execute/async", {"args": [answer, item], "script": """
                  var answer = arguments[0], item = arguments[1], done = arguments[2], delays = [];
                  var realFetch = window.fetch, realTimeout = window.setTimeout;
                  window.fetch = function (url) {
                    if (/\\/api\\/desk\\/items$/.test(url)) return Promise.resolve(new Response(JSON.stringify({ id: item })));
                    if (/\\/start$/.test(url)) return Promise.resolve(new Response(JSON.stringify(answer)));
                    return realFetch.apply(window, arguments);
                  };
                  window.setTimeout = function (f, ms) { delays.push(ms); return realTimeout(f, ms); };
                  document.getElementById('desk-error').innerHTML = '';
                  document.getElementById('desk-pickup-body').innerHTML =
                    '<div class="desk-pick"><button class="btn" type="button" data-pick-start="ABC-1">Start work</button></div>';
                  document.querySelector('[data-pick-start]').click();
                  (function wait(n) {
                    var text = document.getElementById('desk-error').textContent;
                    if (text.indexOf('Worktree') >= 0 || !n) {
                      window.fetch = realFetch; window.setTimeout = realTimeout;
                      done({ text: text, delays: delays });
                    } else realTimeout(function () { wait(n - 1); }, 100);
                  })(50);"""})
            start = {"worktree": "/w/fw-ABC-1", "branch": "abc-1-loader", "how": "reused", "moved_to": None, "warnings": [],
                     "folder_uri": "vscode://file/w/fw-ABC-1"}
            got = start_with(start)
            check("Start work with no prompt link opens only the folder, and says the chat starts there",
                  5000 not in got["delays"] and "its chat starts there" in got["text"], got)
            got = start_with({**start, "claude_uri": "vscode://anthropic.claude-code/open?prompt=x"})
            check("with a prompt link it opens the folder, then the prompt, as before",
                  5000 in got["delays"] and "press Enter there" in got["text"], got)
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
