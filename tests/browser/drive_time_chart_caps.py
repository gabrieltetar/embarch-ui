#!/usr/bin/env python3
"""The Time chart says when a sample strip or a mark lane is capped (ui/086).
Needs stub_core.py, a UI pointed at it, geckodriver (see README.md).

Post-hoc: a completed study whose `rail` has more placeable rows than the strip
cap shows a visible note on the strip; an uncapped one shows none.
Live: a running study's dev-bench console overflows the live ring before any
browser asks. The first view (a snapshot after the drops) says marks are
missing; a lane with no drops says nothing.

Run it against a freshly started UI: the live half needs the stub's LIVEBIG
study to be opened once, and its overflow is sent when the UI's session starts."""
import json, sys, time, urllib.request
import addr

GECKO = addr.gecko_url()
UI = addr.ui_url()
DONE, CAPPED, LIVEBIG = "bb" * 16, "f1" * 16, "f2" * 16


def rq(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(GECKO + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read() or b"{}")


def main():
    for _ in range(50):
        try:
            urllib.request.urlopen(GECKO + "/status", timeout=1); break
        except Exception:
            time.sleep(0.2)
    sid = rq("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "firefox", "moz:firefoxOptions": {"args": ["-headless"]}}}})["value"]["sessionId"]
    base = "/session/" + sid

    def script(js, args=None):
        return rq("POST", base + "/execute/sync", {"script": js, "args": args or []})["value"]

    fails = []
    def check(name, ok, detail=""):
        print(("PASS  " if ok else "FAIL  ") + name + (("  — " + str(detail)) if detail else ""))
        if not ok: fails.append(name)

    def open_study(study, wait):
        rq("POST", base + "/url", {"url": UI})
        time.sleep(1.5)
        script("document.querySelector('.nav-item[data-tab=\"live-study\"]').click()")
        time.sleep(1)
        script("document.getElementById('ls-open-id').value = arguments[0];"
               "document.getElementById('ls-open').click();", [study])
        time.sleep(wait)

    def cap_notes():
        return script("return Array.prototype.map.call(document.querySelectorAll('#tc-chart .tc-cap-note'),"
                      "function(e){return e.textContent;});")

    def tab_notes():
        return script("var e=document.getElementById('tc-notes');"
                      "return e.style.display==='none' ? '' : e.textContent;")

    try:
        # ---- post-hoc strip past its cap ---------------------------------
        open_study(CAPPED, 25)
        notes = cap_notes()
        check("a capped strip shows a visible note on the strip",
              len(notes) == 1 and "capped: 3 sample(s) not drawn" in notes[0]
              and "covers the run up to" in notes[0], notes)
        check("the tab's own note line reports the strip's drops separately",
              "3 sample(s) past a strip's cap" in tab_notes(), tab_notes())
        check("the capped rows are not blamed on the axis (no 'cannot place' text)",
              "cannot place" not in script("return document.getElementById('tc-chart').textContent"))

        # ---- an uncapped one shows nothing --------------------------------
        open_study(DONE, 10)
        check("an uncapped study shows no cap note on a strip or lane",
              cap_notes() == [] and "past a" not in tab_notes(), [cap_notes(), tab_notes()])

        # ---- live lane after a snapshot ----------------------------------
        # First open starts the session, which drains the stub's overflow.
        open_study(LIVEBIG, 8)
        # Reopen: the existing session answers with a snapshot of its ring.
        open_study(LIVEBIG, 8)
        notes = cap_notes()
        check("a live lane whose snapshot reported drops shows a note",
              len(notes) == 1 and "earlier event(s) past the cap, not drawn" in notes[0], notes)
        n = int(notes[0].split(" ")[0]) if notes else 0
        check("the note counts the marks the ring dropped (20500 - 20000)", n == 500, n)
        check("the tab's note line names the missing marks too",
              "past a lane's cap" in tab_notes(), tab_notes())
    finally:
        try:
            rq("DELETE", base)
        except Exception:
            pass
    print("\n%d failed" % len(fails) if fails else "\nall passed")
    sys.exit(1 if fails else 0)


main()
