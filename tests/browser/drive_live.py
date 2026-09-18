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
        # Against embarch-core's own count for *this* study rather than a
        # hardcoded one: the README says to pass any completed study with an
        # outpost tap, and an expectation pinned to one study makes every other
        # one fail for a reason that is not a defect.
        want = len(json.load(urllib.request.urlopen(
            UI + "/api/studies/" + STUDY, timeout=60))["steps"]["steps"])
        check("its steps render off disk", steps == want, str(steps) + " of " + str(want))
        consoles = script("return document.querySelectorAll('#ls-consoles .ls-console').length")
        check("both Text taps get a console: dev-bench and the DUT shell", consoles == 2, consoles)
        check("the DUT shell console is named as its own tap",
              script("return document.getElementById('ls-consoles').innerText.indexOf('nus-shell')>=0"))
        check("the bench console has its captured lines in it",
              script("var e=document.getElementById('ls-console-dev-bench'); return e ? e.children.length : 0") > 0)
        # ---- the Time chart -------------------------------------------
        #
        # The one surface that answers "what was the DUT doing when that
        # notification arrived". Its checks are the alignment ones: every lane
        # draws, a cluster says it is a cluster rather than presenting itself
        # as one event, and clicking a mark opens the same row the Data card
        # below shows for that record.
        check("the Time chart draws on one axis",
              script("return document.getElementById('tc-body').style.display") == "block")
        check("its axis names the clock it is on",
              script("var t=document.getElementById('tc-axis-note').textContent;"
                     "return t.indexOf('embarch-core')>=0 && t.length>80"))
        check("a lane draws per stream, plus the step row above them",
              script("return document.querySelectorAll('#tc-chart rect').length") > 3)
        check("the step row is drawn in the pinned header, not in the body",
              script("return document.querySelectorAll('#tc-head rect').length") > 0)
        # A console lane is drawn either way and **never silently missing**:
        # placed, on a study captured since embarch-core kept a Text tap's
        # arrival sidecar (`embarch-core` decision 73), and otherwise a count
        # with the reason beside it. Both are correct; going missing is not.
        console_lane = script(
            "var t=document.getElementById('tc-chart').textContent;"
            "return [t.indexOf('this chart cannot place')>=0, t.indexOf('console')>=0"
            " || t.indexOf('dev-bench')>=0];")
        check("a console lane is drawn — placed, or saying why it cannot be",
              console_lane[0] or console_lane[1], console_lane)
        clusters = script("return document.querySelectorAll('#tc-chart rect[fill^=\"url(#tc-cluster\"]').length")
        singles = script("return document.querySelectorAll('#tc-chart rect.tc-mark').length")
        check("at full zoom most marks merge, and a cluster is drawn as a cluster",
              clusters > 0 and singles > 0, str(singles) + " single, " + str(clusters) + " merged")
        # Open one mark and confirm it is the row it claims to be.
        script("var m=document.querySelector('#tc-chart rect.tc-mark');"
               "if(m) m.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true,button:0}));")
        time.sleep(3)
        detail = script("return document.getElementById('tc-detail').innerText") or ""
        check("clicking a mark opens the row it is, not a second rendering of it",
              script("return document.getElementById('tc-detail').style.display") == "block"
              and "step_name" in detail.lower(), detail[:80])
        check("an opened mark says which clock placed it and how closely",
              "embarch-core received it at" in detail)

        # The trace checks apply only to a study that declared one. Read off
        # embarch-core's own stream index rather than assumed: the README says
        # to pass any completed study, and three checks that fail on a study
        # with no trace are three checks nobody can read.
        taps = json.load(urllib.request.urlopen(
            UI + "/api/studies/" + STUDY, timeout=60))["taps"] or []
        traced = [t for t in taps if t.get("is_outpost_trace") and t.get("rendered")]
        if traced:
            check("the trace tap selector offers this study's outpost tap",
                  script("var s=document.getElementById('trace-tap'); return !s.disabled && s.options.length>=1"))
            time.sleep(12)
            check("the trace chart draws",
                  script("return document.querySelectorAll('#trace-chart rect, #trace-chart path').length") > 10)
            check("the load repartition renders rows",
                  script("return document.querySelectorAll('#trace-load-rows tr').length") > 0)
        else:
            check("a study with no outpost tap says so instead of an empty chart",
                  script("var s=document.getElementById('trace-tap');"
                         "return s.disabled && s.textContent.indexOf('declared no outpost trace')>=0"))
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
