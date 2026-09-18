#!/usr/bin/env python3
"""Drives embarch-ui's Live Study tab in headless Firefox via geckodriver's
own HTTP API — no selenium package needed on this bench."""
import json, subprocess, sys, time, urllib.request

GECKO = "http://127.0.0.1:4444"
UI = "http://127.0.0.1:4899"


def rq(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(GECKO + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read() or b"{}")


def start():
    for _ in range(50):
        try:
            urllib.request.urlopen(GECKO + "/status", timeout=1)
            return
        except Exception:
            time.sleep(0.2)
    sys.exit("geckodriver never came up")


def main():
    start()
    s = rq("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "firefox",
        "moz:firefoxOptions": {"args": ["-headless"]}}}})
    sid = s["value"]["sessionId"]
    base = "/session/" + sid

    def script(js, args=None):
        return rq("POST", base + "/execute/sync", {"script": js, "args": args or []})["value"]

    failures = []

    def check(name, ok, detail=""):
        print(("PASS  " if ok else "FAIL  ") + name + (("  — " + str(detail)) if detail else ""))
        if not ok:
            failures.append(name)

    try:
        rq("POST", base + "/url", {"url": UI})
        # Capture every console error from here on.
        script("window.__errs = []; window.addEventListener('error', function(e){ window.__errs.push(String(e.message)); });"
               "window.addEventListener('unhandledrejection', function(e){ window.__errs.push('rejection: ' + String(e.reason)); });")
        time.sleep(1.5)

        check("the Live Study nav item exists",
              script("return !!document.querySelector('.nav-item[data-tab=\"live-study\"]')"))
        check("the retired Trace tab is gone",
              script("return !document.querySelector('.nav-item[data-tab=\"trace\"]')"))
        script("document.querySelector('.nav-item[data-tab=\"live-study\"]').click()")
        time.sleep(1.5)

        rows = script("return document.querySelectorAll('#ls-studies-rows tr').length")
        check("the studies list renders every study", rows == 5, rows)
        check("an interrupted study is badged as itself, not as completed or failed",
              script("var t=document.querySelector('#ls-studies-rows').innerText;"
                     "return t.indexOf('interrupted') >= 0"))
        check("an unreadable study says so instead of reading as an empty one",
              script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
                     "for (var i=0;i<rows.length;i++){ if (rows[i].innerText.indexOf('not readable')>=0) return true; }"
                     "return false;"))

        # --- open the completed study: steps, consoles, data, no trace -----
        script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
               "for (var i=0;i<rows.length;i++){ if (rows[i].getAttribute('data-study').indexOf('bbbb')===0) { rows[i].click(); return; } }")
        time.sleep(2.5)
        check("its steps render off disk",
              script("return document.querySelectorAll('#ls-steps-rows tr').length") == 2)
        check("a failed step reads as failed, with dev-bench's own reason",
              script("var t=document.querySelector('#ls-steps-rows').innerText;"
                     "return t.indexOf('Fail')>=0 && t.indexOf('ERR_PERMISSION')>=0"))
        check("a Text tap gets a console card",
              script("return document.querySelectorAll('#ls-consoles .ls-console').length") == 1)
        check("the console's last line, with no newline, is shown as partial",
              script("return !!document.querySelector('#ls-consoles .ls-console-partial')"))
        check("a study with no outpost trace says so rather than drawing one",
              script("return document.getElementById('trace-tap').disabled === true"))
        check("the Samples and Raw taps each get a data card",
              script("return document.querySelectorAll('#ls-data > .card').length") == 2)
        check("the samples table renders rows the browser never parsed",
              script("return document.querySelectorAll('[id$=\"-rows\"] tr').length") > 2)
        check("the samples plot draws",
              script("return !!document.querySelector('#ls-data .ls-plot')"))
        check("a Raw tap gets a hex head and no table",
              script("var t=document.querySelector('#ls-data').innerText;"
                     "return t.indexOf('nothing declared to decode them as')>=0"))

        # --- a study with a console and nothing else -----------------------
        script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
               "for (var i=0;i<rows.length;i++){ if (rows[i].getAttribute('data-study').indexOf('eeee')===0) { rows[i].click(); return; } }")
        time.sleep(2)
        check("a study with no data taps shows no empty data card",
              script("return document.querySelectorAll('#ls-data > .card').length") == 0)

        # --- the running study: live frames, lagged, partial console -------
        script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
               "for (var i=0;i<rows.length;i++){ if (rows[i].getAttribute('data-study').indexOf('aaaa')===0) { rows[i].click(); return; } }")
        time.sleep(3.0)
        check("the event feed fills from the live stream",
              script("return document.querySelectorAll('#ls-feed .ls-feed-row').length") > 0)
        check("lagged is displayed, never swallowed",
              script("return document.getElementById('ls-status-lagged').style.display !== 'none' && "
                     "document.getElementById('ls-status-lagged').innerText.indexOf('7')>=0"))
        check("a chunk that ended mid-line is still shown as partial, live",
              script("return !!document.querySelector('#ls-consoles .ls-console-partial')"))
        check("a line split across two chunks is assembled, not duplicated",
              script("var t=document.querySelector('#ls-consoles .ls-console').innerText;"
                     "return t.indexOf('uart:~$ half a line')>=0"))
        check("the live sample plot is labelled a preview",
              script("return document.querySelector('#ls-data').innerText.indexOf('live preview')>=0"))
        check("a terminal status lands and names the failing step",
              script("var b=document.getElementById('ls-status-badge');"
                     "return b.innerText.indexOf('failed')>=0"))

        # --- reload mid-run replays the run so far -------------------------
        rq("POST", base + "/url", {"url": UI})
        time.sleep(1.0)
        script("document.querySelector('.nav-item[data-tab=\"live-study\"]').click()")
        time.sleep(1.0)
        script("window.__lsReopen = true;")
        script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
               "for (var i=0;i<rows.length;i++){ if (rows[i].getAttribute('data-study').indexOf('aaaa')===0) { rows[i].click(); return; } }")
        time.sleep(2.5)
        check("reopening the same study replays the whole run from the server's rings",
              script("return document.querySelectorAll('#ls-steps-rows tr').length") >= 2)

        errs = script("return window.__errs || []")
        check("no uncaught JavaScript error", not errs, errs)
    finally:
        try:
            rq("DELETE", base)
        except Exception:
            pass

    print()
    if failures:
        print(str(len(failures)) + " check(s) FAILED: " + ", ".join(failures))
        sys.exit(1)
    print("every check passed")


if __name__ == "__main__":
    main()
