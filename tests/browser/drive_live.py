#!/usr/bin/env python3
"""Opens one real, completed study in the Live Study tab and checks every card
renders against embarch-core's own data."""
import json, sys, time, urllib.request

GECKO = "http://127.0.0.1:4444"
UI = "http://127.0.0.1:4899"
STUDY = sys.argv[1]


def rq(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(GECKO + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read() or b"{}")


def main():
    for _ in range(60):
        try:
            urllib.request.urlopen(GECKO + "/status", timeout=1); break
        except Exception:
            time.sleep(0.2)
    s = rq("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "firefox", "moz:firefoxOptions": {"args": ["-headless"]}}}})
    sid = s["value"]["sessionId"]
    base = "/session/" + sid

    def script(js, args=None):
        return rq("POST", base + "/execute/sync", {"script": js, "args": args or []})["value"]

    fails = []
    def check(name, ok, detail=""):
        print(("PASS  " if ok else "FAIL  ") + name + (("  — " + str(detail)) if detail else ""))
        if not ok: fails.append(name)

    try:
        rq("POST", base + "/url", {"url": UI})
        script("window.__errs=[]; window.addEventListener('error',function(e){window.__errs.push(String(e.message));});"
               "window.addEventListener('unhandledrejection',function(e){window.__errs.push('rejection: '+String(e.reason));});")
        time.sleep(2)
        script("document.querySelector('.nav-item[data-tab=\"live-study\"]').click()")
        time.sleep(2.5)

        n = script("return document.querySelectorAll('#ls-studies-rows tr').length")
        check("the studies list shows embarch-core's own retention window", n == 50, n)
        check("the saved-study picker is populated from the open project",
              script("return document.getElementById('ls-study-picker').options.length") > 5)

        script("document.getElementById('ls-open-id').value = arguments[0];"
               "document.getElementById('ls-open').click();", [STUDY])
        time.sleep(8)

        steps = script("return document.querySelectorAll('#ls-steps-rows tr').length")
        check("its steps render off disk", steps == 11, steps)
        consoles = script("return document.querySelectorAll('#ls-consoles .ls-console').length")
        check("both Text taps get a console: dev-bench and the DUT shell", consoles == 2, consoles)
        check("the DUT shell console is named as its own tap",
              script("return document.getElementById('ls-consoles').innerText.indexOf('nus-shell')>=0"))
        check("the bench console has its captured lines in it",
              script("var e=document.getElementById('ls-console-dev-bench'); return e ? e.children.length : 0") > 0)
        check("the trace tap selector offers this study's outpost tap",
              script("var s=document.getElementById('trace-tap'); return !s.disabled && s.options.length>=1"))
        time.sleep(12)
        check("the trace chart draws",
              script("return document.querySelectorAll('#trace-chart rect, #trace-chart path').length") > 10)
        check("the load repartition renders rows",
              script("return document.querySelectorAll('#trace-load-rows tr').length") > 0)
        cards = script("return document.querySelectorAll('#ls-data > .card').length")
        check("one data card per remaining tap (2 Raw + 1 GattTranscript)", cards == 3, cards)
        check("the GATT transcript renders as a table the browser never parsed",
              script("return document.querySelectorAll('#ls-data-gatt-rows tr').length") > 0)
        check("a Raw tap gets a hex head, not a table",
              script("return document.getElementById('ls-data').innerText.indexOf('nothing declared to decode them as')>=0"))
        check("the captured-streams table reports each tap's bytes and whether it is short",
              script("var t=document.getElementById('ls-streams').innerText;"
                     "return t.indexOf('nus-shell')>=0 && t.indexOf('short of what the source produced')>=0"))
        check("provenance renders, with an unverified version marked unverified",
              script("var t=document.getElementById('ls-provenance').innerText;"
                     "return t.indexOf('dev-bench')>=0 && t.indexOf('unverified')>=0"))
        errs = script("return window.__errs || []")
        check("no uncaught JavaScript error", not errs, errs)
    finally:
        try: rq("DELETE", base)
        except Exception: pass

    print()
    if fails:
        print(str(len(fails)) + " check(s) FAILED: " + ", ".join(fails)); sys.exit(1)
    print("every check passed")


main()
