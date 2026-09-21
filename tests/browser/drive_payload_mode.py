#!/usr/bin/env python3
"""The Study Designer row editor's "Payload as" toggle, in a real browser.

`drive_build.py`'s sibling: same geckodriver/no-stub setup, a different
corner of the Study Designer tab. It covers one bug: flipping "Payload as"
between bytes and text used to change only the select's own value and leave
whatever was already typed sitting under the new mode, unconverted
(`assets/app.js`'s `onSdRowInput`, the `rawMode` branch). Bytes typed as
`0x62 0x6c 0x65 0x0a` stayed exactly that string once "text" was picked,
rather than becoming the word `ble` followed by a newline escape — silently
garbling whatever payload was authored.

This drives one `raw` row through both directions of the round trip
(`parseBytes`/`bytesToHexTokens`/`bytesToEscapedText` in `assets/app.js`),
plus the non-UTF8 edge case the bytes-to-text direction has to survive
without throwing.

Unlike `drive_build.py`, the row table only exists once a project is open
(`sdEnterProject`'s early return otherwise), so this opens a throwaway repo
whose only requirement is a `.git` directory — the same minimum
`drive_topology.py` relies on — through the same route the Study Designer's
own "Open project" panel posts to.

Run it the same way as `drive_build.py`:

    cargo build --release
    geckodriver --port 4444 &
    EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
    python3 tests/browser/drive_payload_mode.py
"""
import json, os, subprocess, urllib.request, sys, time

UI = "http://127.0.0.1:" + os.environ.get("EMBARCH_UI_PORT", "4899")
BASE = "http://127.0.0.1:4444"
REPO = "/tmp/embarch-ui-payload-mode-drive-repo"


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
# preloads its own four rows into a fresh project, and a bare selector
# would silently grab the first of those instead of the one this test adds.
ACTION_SEL = "document.querySelector('#sd-rows tr:last-child select[data-field=\"action\"]')"
MODE_SEL = "document.querySelector('#sd-rows tr:last-child select[data-field=\"rawMode\"]')"
PAYLOAD_SEL = "document.querySelector('#sd-rows tr:last-child input[data-field=\"rawPayload\"]')"

try:
    rq("POST", b + "/url", {"url": UI + "/"})
    script("window.__errs=[];window.addEventListener('error',"
           "function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)
    script("document.querySelector('.nav-item[data-tab=\"study-designer\"]').click();")
    time.sleep(3)

    # A new row, switched to the "Raw GATT" one-off kind — no discovery data
    # needed, unlike Registered/Vendor-defined.
    script("document.getElementById('sd-add-row').click();")
    time.sleep(0.5)
    check("the added row's action select is found",
          script("return !!" + ACTION_SEL + ";"))
    script(ACTION_SEL + ".value='raw:';" + ACTION_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(0.3)

    check("the row starts in text mode with an empty payload",
          script("return " + MODE_SEL + ".value === 'text' && " + PAYLOAD_SEL + ".value === '';"))

    # Type a payload as text, using the \n escape `parseBytes("text")` already
    # understands — this is the same shape as a real NUS command.
    script(PAYLOAD_SEL + ".value=arguments[0];" + PAYLOAD_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));",
           ["ble\\n"])
    check("the typed text payload is there before any toggle",
          script("return " + PAYLOAD_SEL + ".value;") == "ble\\n")

    # text -> bytes: "ble\n" is 0x62 0x6c 0x65 0x0a.
    script(MODE_SEL + ".value='hex';" + MODE_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(0.2)
    hexval = script("return " + PAYLOAD_SEL + ".value;")
    check("text -> bytes converts what was typed, not just the mode label",
          hexval == "0x62 0x6c 0x65 0x0a", hexval)

    # bytes -> text again: round-trips back to the original escape form.
    script(MODE_SEL + ".value='text';" + MODE_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(0.2)
    textval = script("return " + PAYLOAD_SEL + ".value;")
    check("bytes -> text round-trips back to the typed escape form",
          textval == "ble\\n", textval)

    # Non-UTF8 bytes: 0xff is not a valid UTF-8 lead byte on its own. Going
    # to text has to decode it lossily, not throw.
    script(MODE_SEL + ".value='hex';" + MODE_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(0.2)
    script(PAYLOAD_SEL + ".value=arguments[0];" + PAYLOAD_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));",
           ["0xff 0x41"])
    script(MODE_SEL + ".value='text';" + MODE_SEL + ".dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(0.2)
    lossy = script("return " + PAYLOAD_SEL + ".value;")
    check("a non-UTF8 byte decodes lossily instead of throwing",
          lossy.endswith("A") and "�" in lossy, repr(lossy))

    errs = script("return window.__errs;")
    check("no uncaught JavaScript error", not errs, errs)
finally:
    rq("DELETE", b, None)
print(("\n%d FAILED" % len(fails)) if fails else "\nall passed")
sys.exit(1 if fails else 0)
