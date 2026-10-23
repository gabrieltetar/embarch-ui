"""The Desk tab, in a real browser, against a scratch embarch-desk.

Starts its own embarch-desk on a scratch port with scratch config and data
directories (so the real desk, its tokens and its state are never touched),
seeds it through the desk's own HTTP surface — a standalone work item, a
session that a hook puts on it, a commit-less line, a flash — then starts an
embarch-ui pointed at that desk and a dead Core, and drives the tab: the card
and its tile render, ✓ on a tile nobody has opened opens it instead of
sending, the dialog shows the lines, ✕ discards, the Dashboard is gone. The
chat's part (embarch-chat-desk-proposal.md): a session's row before the chat
ships and its one line after, Open chat through the mailbox (an ended chat
too), the card's notes from memory files written in a scratch Claude folder,
read again only when a note moves, the chat setting from the desk's view,
Start work with and without a prompt link, and a proxy that answers only the
UI's own page.

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

        def hook(ev):
            subprocess.run([DESK_BIN, "hook"], input=json.dumps(ev), env=env, capture_output=True, text=True)

        def mailbox_for(folder):
            return desk("GET", "/mailbox?folder=" + urllib.parse.quote(folder), token=token)["entries"]

        repo = scratch / "repo"
        repo.mkdir()
        item = desk("POST", "/items", {"title": "Client sync call"}, token)["id"]
        start = {"hook_event_name": "SessionStart", "session_id": "s1", "cwd": str(repo)}
        out = subprocess.run([DESK_BIN, "hook"], input=json.dumps(start), env=env, capture_output=True, text=True).stdout
        check("a new session is asked which work item it is", "desk_set_work_item" in out, out[:120])
        desk("POST", "/sessions/s1/assign", {"item_id": item}, token)
        desk("POST", "/sessions/s1/line", {"text": "Agreed the bootload test plan"}, token)
        hook({"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo),
              "tool_name": "mcp__embarch-api__flash", "tool_input": {}, "tool_response": {}})
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

            def until(src, secs=20):
                v = None
                for _ in range(secs * 5):
                    v = js(src)
                    if v:
                        return v
                    time.sleep(0.2)
                return v

            def reload_view():
                # The nav item reads /view at once, rather than waiting for the next push.
                js("document.querySelector('.nav-item[data-tab=desk]').click(); return true")

            def real_click(css):
                el = wd("POST", S + "/element", {"using": "css selector", "value": css})
                wd("POST", S + "/element/" + list(el.values())[0] + "/click", {})

            def stub_fetch(pattern, status, error):
                # Answer one route with an error, once per call, until unstub_fetch().
                js("var re = new RegExp(arguments[0]), st = arguments[1], msg = arguments[2];"
                   "window.__realFetch = window.__realFetch || window.fetch;"
                   "window.fetch = function (u) { if (re.test(u)) return Promise.resolve(new Response(JSON.stringify({error: msg}), {status: st}));"
                   " return window.__realFetch.apply(window, arguments); }; return true", pattern, status, error)

            def unstub_fetch():
                js("if (window.__realFetch) window.fetch = window.__realFetch; return true")

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
            # Until the chat ships (chat decision 4) the setting is off, and a
            # session keeps its row from before: Answer, Jump and its last reply.
            for ev in ({"hook_event_name": "Stop", "session_id": "s1", "cwd": str(repo),
                        "last_assistant_message": "Summary: the loader needs 432 KB."},
                       {"hook_event_name": "Notification", "session_id": "s1", "cwd": str(repo),
                        "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}):
                hook(ev)
            row_js = ("var r = document.querySelector('.desk-session'); var a = r && r.querySelector('a');"
                      " return r && {text: r.textContent, answer: a ? a.getAttribute('href') : null,"
                      " reply: (r.querySelector('.desk-session-reply') || {}).textContent || '',"
                      " jump: !!r.querySelector('[data-jump-session]'), chat: !!r.querySelector('[data-open-chat]'),"
                      " line: !!r.querySelector('.desk-session-state'), links: r.querySelectorAll('a').length}")
            row = until(row_js.replace("return r &&", "return r && r.textContent.indexOf('432 KB') >= 0 &&"))
            check("with the chat setting off, a session keeps Answer, Jump and its last reply (chat decision 4)",
                  bool(row) and (row["answer"] or "").endswith("/code/s1") and row["jump"] and not row["chat"]
                  and "432 KB" in row["reply"] and not row["line"], row)

            # The chat setting is the desk's, rendered from each view it pushes.
            js("document.getElementById('desk-settings-toggle').click(); return true")
            toggle = until("var c = document.getElementById('desk-chat-panel'); return c && !c.disabled && {on: c.checked}")
            check("the chat setting shows in the panel, off by default", toggle == {"on": False}, toggle)
            desk("PUT", "/settings", {"chat_panel": True}, token)
            reload_view()
            check("the setting changed elsewhere reaches the open panel with the next view",
                  until("return document.getElementById('desk-chat-panel').checked") is True)
            desk("PUT", "/settings", {"chat_panel": False}, token)
            reload_view()
            until("return !document.getElementById('desk-chat-panel').checked")
            # A real click focuses the box; that must not hold every pushed view back.
            real_click("#desk-chat-panel")
            on = False
            for _ in range(30):
                if desk("GET", "/settings", token=token).get("chat_panel") is True:
                    on = True
                    break
                time.sleep(0.2)
            check("turning the setting on with a click reaches the desk", on)
            check("and leaves no focus in the panel", until("return !document.getElementById('desk-settings').contains(document.activeElement)", 5) is True,
                  js("return document.activeElement && document.activeElement.id"))
            pushed_id = desk("POST", "/items", {"title": "Pushed after the toggle"}, token)["id"]
            check("a card made elsewhere after the click still arrives by SSE",
                  until("return document.getElementById('desk-cards').textContent.indexOf('Pushed after the toggle') >= 0", 25) is True)
            desk("POST", "/items/%s/archive" % pushed_id, {"value": True}, token)   # out of the way of the tile checks

            # With it on, a session is one line — state, what it waits for, age
            # — with Jump; Answer and the last reply have left it (chat decision 32).
            row = until(row_js.replace("return r &&", "return r && !!r.querySelector('.desk-session-state') &&"))
            check("with it on, a terminal session is one line: its state, what it waits for, its age, and Jump",
                  bool(row) and "waiting" in row["text"] and "for permission prompt" in row["text"]
                  and ("ago" in row["text"] or "just now" in row["text"]) and row["jump"] and not row["chat"], row)
            check("Answer and the last reply have left the row", bool(row) and row["links"] == 0 and "432 KB" not in row["text"], row)
            js("document.getElementById('desk-settings-toggle').click(); return true")

            # A chat the panel runs gets Open chat: a focus request left in
            # the desk's mailbox for its folder, then that folder's link.
            chat_dir = scratch / "repo-ABC-1"
            chat_dir.mkdir()
            desk("POST", "/sessions/c1/assign", {"item_id": item, "client": "embarch-chat"}, token)
            hook({"hook_event_name": "SessionStart", "session_id": "c1", "cwd": str(chat_dir)})
            rows = None
            for _ in range(100):
                rows = js("return Array.from(document.querySelectorAll('.desk-session')).map(function (r) {"
                          " return {chat: !!r.querySelector('[data-open-chat]'), jump: !!r.querySelector('[data-jump-session]')}; })")
                if any(r["chat"] for r in rows):
                    break
                time.sleep(0.2)
            check("a chat the panel runs gets Open chat, and only the other session Jump",
                  sorted((r["chat"], r["jump"]) for r in rows) == [(False, True), (True, False)], rows)

            def mailbox():
                return desk("GET", "/mailbox?folder=" + urllib.parse.quote(str(chat_dir)), token=token)["entries"]

            js("document.querySelector('[data-open-chat]').click(); return true")
            mail = []
            for _ in range(30):
                mail = mailbox()
                if mail:
                    break
                time.sleep(0.2)
            check("Open chat leaves a focus request for the chat's folder in the desk's mailbox",
                  [(m["kind"], m["session_id"]) for m in mail] == [("focus", "c1")], mail)
            for m in mail:
                desk("POST", "/mailbox/%s/take" % m["id"], {}, token)

            # A mailbox that refuses: the error shows and no link is followed.
            stub_fetch("/api/desk/mailbox$", 500, "the mailbox refused it")
            href = js("return location.href")
            js("document.querySelector('[data-open-chat]').click(); return true")
            check("Open chat that the desk refuses says why and opens nothing",
                  until("return document.getElementById('desk-error').textContent.indexOf('the mailbox refused it') >= 0", 5) is True
                  and js("return location.href") == href)
            unstub_fetch()

            # A chat whose window closed keeps its line, and Open chat resumes it (32).
            hook({"hook_event_name": "SessionEnd", "session_id": "c1", "cwd": str(chat_dir)})
            reload_view()
            ended = until("var b = document.querySelector('.desk-session.ended [data-open-chat=c1]'); return b && b.title")
            check("an ended chat keeps its line on the card, with Open chat to resume it", bool(ended) and "resume" in ended, ended)
            mail = []
            if js("var b = document.querySelector('.desk-session.ended [data-open-chat=c1]'); if (b) b.click(); return !!b"):
                for _ in range(30):
                    mail = mailbox()
                    if mail:
                        break
                    time.sleep(0.2)
            check("Open chat on an ended chat leaves the focus request its window resumes it from",
                  [(m["kind"], m["session_id"]) for m in mail] == [("focus", "c1")], mail)
            for m in mail:
                desk("POST", "/mailbox/%s/take" % m["id"], {}, token)
            # With the setting off the rows are as before the chat: no ended chat.
            desk("PUT", "/settings", {"chat_panel": False}, token)
            reload_view()
            off = until("var r = document.querySelectorAll('.desk-session'); return r.length === 1 && r[0].querySelector('a') && r.length")
            check("with the setting off again, the ended chat stays off the card and Answer is back", off == 1,
                  js("return Array.from(document.querySelectorAll('.desk-session')).map(function (r) { return r.textContent; })"))
            desk("PUT", "/settings", {"chat_panel": True}, token)
            reload_view()

            # Notes: each at its newest state on the card, every one behind a
            # click (chat decision 46). A memory file written twice keeps both
            # versions; the card shows the second.
            mem = scratch / "claude" / "projects" / "-w-fw" / "memory"
            mem.mkdir(parents=True)

            def write_mem(name, text, tool="Write"):
                path = mem / name
                path.write_text(text)
                hook({"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo), "tool_name": tool,
                      "tool_input": {"file_path": str(path)}, "tool_response": {}})

            write_mem("abc-1-state.md", '---\nname: abc-1-state\ndescription: "ABC-1: branch made"\n---\n\nBranch abc-1-loader.\n')
            write_mem("abc-1-state.md", '---\nname: abc-1-state\ndescription: "ABC-1: PR open, flash pending"\n---\n\nBranch abc-1-loader, PR open.\n', "Edit")
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
            check("the card's notes have a short accessible name",
                  (js("return document.querySelector('.desk-notes').getAttribute('aria-label')") or "").startswith("Notes, "))
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
            a11y = js("var d = document.getElementById('desk-notes'), t = document.getElementById(d.getAttribute('aria-labelledby'));"
                      " return {name: t ? t.textContent : null, focus: document.activeElement.id}")
            check("the notes dialog is named by its title and takes focus on Close",
                  (a11y["name"] or "").startswith("Notes — ") and a11y["focus"] == "desk-notes-close", a11y)
            if SHOT:
                Path(SHOT.replace(".png", "-notes.png")).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))
            js("document.getElementById('desk-notes-close').click(); return true")
            check("Close puts the list away", js("return document.getElementById('desk-notes').hidden"))
            check("and gives focus back to the card's notes", js("return document.activeElement.getAttribute('data-notes')") == item)

            # What a memory file or a line says renders as text, never as markup;
            # a YAML-quoted or folded description reads as its writer meant it.
            write_mem("x<img src=q onerror=window.__p0=1>.md",
                      '---\nname: x\ndescription: "<img src=x onerror=window.__p1=1>"\n---\n\n<script>window.__p2=1</script><b>bold</b>\n')
            desk("POST", "/sessions/s1/line", {"text": "<img src=y onerror=window.__p3=1> a line", "kind": "decision"}, token)
            write_mem("quoted.md", '---\nname: quoted\ndescription: "folded into \\"budget the ports\\" and pushed"\n---\n\nbody\n')
            write_mem("folded.md", "---\nname: folded\ndescription: >-\n  a description folded\n  across two lines\ntype: project\n---\n\nbody\n")
            card = until("var n = document.querySelector('.desk-notes'); return n && n.textContent.indexOf('across two lines') >= 0 && n.textContent") \
                or js("var n = document.querySelector('.desk-notes'); return n ? n.textContent : ''")
            check("a quoted description shows its quotes, not their escapes",
                  bool(card) and 'folded into "budget the ports" and pushed' in card and '\\"' not in card, card)
            check("a folded description shows its text, not its marker", bool(card) and "a description folded across two lines" in card, card)
            injected = "return {el: document.querySelectorAll(arguments[0] + ' img, ' + arguments[0] + ' script, ' + arguments[0] + ' .desk-note b, ' + arguments[0] + ' .desk-notes-text b').length," \
                       " p: [window.__p0, window.__p1, window.__p2, window.__p3].filter(function (x) { return x !== undefined; }).length}"
            got = js(injected, "#desk-cards")
            check("a memory file's name, description and a line render as text on the card", got == {"el": 0, "p": 0} and "<img src=x" in card, got)
            js("document.querySelector('.desk-notes').click(); return true")
            until("return document.getElementById('desk-notes').textContent.indexOf('window.__p2') >= 0")
            got = js(injected, "#desk-notes")
            check("and in the full list", got == {"el": 0, "p": 0}, got)
            js("document.getElementById('desk-notes-close').click(); return true")

            # Many memory files: the card shows the three newest, and counts the rest.
            write_mem("topic-1.md", '---\nname: t1\ndescription: "topic one"\n---\n\nbody\n')
            write_mem("topic-2.md", '---\nname: t2\ndescription: "topic two"\n---\n\nbody\n')
            files = until("var n = document.querySelector('.desk-notes'); return n && n.textContent.indexOf('topic two') >= 0 &&"
                          " {rows: n.querySelectorAll('.desk-note-file').length, label: n.querySelector('.desk-label').textContent}")
            check("a card shows its three newest memory files and counts the rest in its label",
                  bool(files) and files["rows"] == 3 and "3 more files" in files["label"], files)

            # A card reads its notes again only when a note moves notes_ms, not
            # on every hook; a read that fails is made again on the next view.
            js("window.__reads = 0; window.__failReads = 0; var f = window.fetch;"
               " window.fetch = function (u) { if (/\\/notes$/.test(u)) { window.__reads++;"
               "   if (window.__failReads > 0) { window.__failReads--; return Promise.resolve(new Response('{}', {status: 502})); } }"
               " return f.apply(window, arguments); }; return true")
            for _ in range(3):
                hook({"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo), "tool_name": "Read",
                      "tool_input": {"file_path": "/etc/hostname"}, "tool_response": {}})
                reload_view()
                settle()
            check("hooks that write no note do not make the card read its notes again", js("return window.__reads") == 0,
                  js("return window.__reads"))
            desk("POST", "/sessions/s1/line", {"text": "Read once when a note moves", "kind": "todo"}, token)
            reload_view()
            shown = until("return document.querySelector('.desk-notes').textContent.indexOf('Read once when a note moves') >= 0")
            settle()
            check("a new note makes the card read them once", bool(shown) and js("return window.__reads") == 1, js("return window.__reads"))
            js("window.__failReads = 1; return true")
            desk("POST", "/sessions/s1/line", {"text": "Read again after a failed read", "kind": "handoff"}, token)
            reload_view()
            until("return window.__reads >= 2")
            settle()
            reload_view()
            check("a failed read is made again on the next view",
                  until("return document.querySelector('.desk-notes').textContent.indexOf('Read again after a failed read') >= 0") is True,
                  js("return window.__reads"))
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
            # A save the desk refuses puts the box back and says why.
            was = js("return document.getElementById('desk-chat-panel').checked")
            stub_fetch("/api/desk/settings$", 500, "settings refused")
            js("document.getElementById('desk-chat-panel').click(); return true")
            said = until("return document.getElementById('desk-error').textContent.indexOf('settings refused') >= 0", 5)
            check("a refused save of the chat setting puts the box back and says why",
                  said is True and js("return document.getElementById('desk-chat-panel').checked") == was
                  and desk("GET", "/settings", token=token).get("chat_panel") == was)
            unstub_fetch()

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

            # The proxy carries the desk's token, so it answers only this UI's
            # own page (desk review r1, finding 2). A plain-text POST needs no
            # CORS preflight: from the UI's own page it reaches the desk as the
            # text it is, and the desk's JSON route refuses it.
            evil = "/w/from-elsewhere"
            got = wd("POST", S + "/execute/async", {"args": [evil], "script": """
              var done = arguments[1];
              fetch('/api/desk/mailbox', {method: 'POST', body: JSON.stringify({kind: 'focus', session_id: 'x1', folder: arguments[0]})})
                .then(function (r) { done(r.status); }, function (e) { done(String(e)); });"""})
            check("a plain-text body reaches the desk as text, and its JSON route refuses it",
                  got == 415 and mailbox_for(evil) == [], got)
            # From another origin (localhost is not 127.0.0.1), with no-cors.
            wd("POST", S + "/url", {"url": "http://localhost:" + UI_PORT + "/"})
            wd("POST", S + "/execute/async", {"args": [UI, evil], "script": """
              var done = arguments[2];
              fetch(arguments[0] + '/api/desk/mailbox', {method: 'POST', mode: 'no-cors',
                    headers: {'Content-Type': 'text/plain'},
                    body: JSON.stringify({kind: 'focus', session_id: 'x1', folder: arguments[1]})})
                .then(function () { done(true); }, function () { done(true); });"""})
            check("a page on another origin cannot reach the desk through the proxy", mailbox_for(evil) == [])
        finally:
            wd("DELETE", S)
        req = urllib.request.Request(UI + "/api/desk/mailbox", data=b'{"kind":"focus","session_id":"x1","folder":"/w/x"}', method="POST",
                                     headers={"Content-Type": "application/json", "Sec-Fetch-Site": "cross-site"})
        try:
            urllib.request.urlopen(req, timeout=5)
            check("a cross-site request to the proxy is refused", False)
        except urllib.error.HTTPError as e:
            check("a cross-site request to the proxy is refused", e.code == 403 and mailbox_for("/w/x") == [], e.code)

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
