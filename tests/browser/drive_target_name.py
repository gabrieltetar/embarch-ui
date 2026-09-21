#!/usr/bin/env python3
"""The BleConnect row's "Device name" box, in a real browser.

`drive_payload_mode.py`'s sibling: same geckodriver/no-stub setup, a
different corner of the Study Designer tab. It covers one bug: typing into
the "Device name" box on a `ble_connect` row lost focus after the very first
keystroke into an empty box. `onSdRowInput`'s `targetName` branch tracked
whether the box was blank before and after each keystroke and called the
full `renderSdRows()` whenever that flipped — which is exactly what happens
on character one — regenerating the whole table's DOM and dropping focus and
the caret mid-word. `updateSdDelayHints` already existed as the fix pattern
for the analogous "patch a hint in place, don't re-render the table" problem
one field over; this box just hadn't been given the same treatment yet.

Unlike `drive_build.py`, the row table only exists once a project is open
(`sdEnterProject`'s early return otherwise), so this opens a throwaway repo
whose only requirement is a `.git` directory — the same minimum
`drive_topology.py`/`drive_payload_mode.py` rely on.

Run it the same way as `drive_payload_mode.py`:

    cargo build --release
    geckodriver --port 4444 &
    EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
    python3 tests/browser/drive_target_name.py
"""
import json, os, subprocess, urllib.request, sys, time

UI = "http://127.0.0.1:" + os.environ.get("EMBARCH_UI_PORT", "4899")
BASE = "http://127.0.0.1:4444"
REPO = "/tmp/embarch-ui-target-name-drive-repo"


def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]


def ui_post(path, body):
    req = urllib.request.Request(
        UI + path, data=json.dumps(body).encode(), method="POST",
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode()


fails = []


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


# `.git` alone is enough for the open-project check (`api_open_project`'s
# own rule) — no `embarch.toml`, no board/app tree needed for row editing.
subprocess.run(["rm", "-rf", REPO], check=False)
os.makedirs(REPO + "/.git", exist_ok=True)
ui_post("/api/study-designer/project", {"path": REPO})

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
    "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid


def script(js, args=None):
    return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})


# The new row is always the last `<tr>` — the capture-window template
# preloads its own four rows into a fresh project, and a bare selector would
# silently grab the first of those instead of the one this test adds.
ACTION_SEL = "document.querySelector('#sd-rows tr:last-child select[data-field=\"action\"]')"
NAME_SEL = "document.querySelector('#sd-rows tr:last-child input[data-field=\"targetName\"]')"
HINT_SEL = "document.querySelector('#sd-rows tr:last-child .sd-target-name-hint')"

try:
    rq("POST", b + "/url", {"url": UI + "/"})
    script("window.__errs=[];window.addEventListener('error',"
           "function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)
    script("document.querySelector('.nav-item[data-tab=\"study-designer\"]').click();")
    time.sleep(3)

    script("document.getElementById('sd-add-row').click();")
    time.sleep(0.5)
    script(ACTION_SEL + ".value='builtin:ble_connect';" +
           ACTION_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(0.3)

    check("the device-name box is found", script("return !!" + NAME_SEL + ";"))
    check("the hint reads 'any device' on a blank box",
          script("return " + HINT_SEL + ".textContent;").strip() == "— any device!")

    # Type character by character, checking focus survives every one — the
    # bug fired specifically on the blank -> non-blank transition, i.e. the
    # very first character.
    target = "the client S11"
    for i, ch in enumerate(target):
        script(NAME_SEL + ".focus();" + NAME_SEL + ".value += arguments[0];" +
               NAME_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));", [ch])
        still_focused = script("return document.activeElement === " + NAME_SEL + ";")
        check("focus survives keystroke %d ('%s')" % (i + 1, ch), still_focused)
        if not still_focused:
            break

    check("the full name is in the box", script("return " + NAME_SEL + ".value;") == target)
    check("the hint clears once non-blank",
          script("return " + HINT_SEL + ".textContent;") == "")

    # Clear it back to blank: the other direction of the transition this
    # code patches in place rather than re-rendering for.
    script(NAME_SEL + ".value='';" + NAME_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    check("the hint reappears once blank again",
          script("return " + HINT_SEL + ".textContent;").strip() == "— any device!")

    errs = script("return window.__errs;")
    check("no uncaught JavaScript error", not errs, errs)
finally:
    rq("DELETE", b, None)
print(("\n%d FAILED" % len(fails)) if fails else "\nall passed")
sys.exit(1 if fails else 0)
