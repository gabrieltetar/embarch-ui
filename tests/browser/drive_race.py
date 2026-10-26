#!/usr/bin/env python3
"""A slow read for a study that is no longer open must not render under the
one that is (ui/083). Needs stub_core.py, a UI pointed at it, geckodriver.

The browser delays its own `fetch` of one study record, opens a second study
meanwhile, and checks only the second study is on screen once both returned.
Also checks the saved-study picker is re-read on a project switch (the project
switch itself is checked by hand: it needs a real repo).

Run it against a freshly started UI: the second half needs the stub's running
study to be live in the UI's session, which it is only once."""
import json, sys, time, urllib.request

GECKO = "http://127.0.0.1:4444"
UI = "http://127.0.0.1:4899"
RUNNING, DONE, BARE = "aa" * 16, "bb" * 16, "ee" * 16


def rq(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(GECKO + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
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

    def script(js):
        return rq("POST", base + "/execute/sync", {"script": js, "args": []})["value"]

    fails = []
    def check(name, ok, detail=""):
        print(("PASS  " if ok else "FAIL  ") + name + (("  — " + str(detail)) if detail else ""))
        if not ok: fails.append(name)

    def delay_record(study, nth, ms):
        # Delays the nth (1-based) read of /api/studies/<study> by ms.
        script("window.__n=0; window.__seen=0; if(!window.__f){window.__f=window.fetch;}"
               "window.fetch=function(u,o){ if(String(u)==='/api/studies/%s'){"
               "window.__seen++; if(window.__seen===%d){return new Promise(function(r){"
               "setTimeout(function(){r(window.__f(u,o));},%d);});}} return window.__f(u,o);};"
               % (study, nth, ms))

    def on_screen():
        return script("return {mode:document.getElementById('ls-status-mode').textContent,id:document.getElementById('ls-status-id').textContent,"
                      "name:document.getElementById('ls-status-name').textContent,"
                      "steps:Array.prototype.map.call(document.querySelectorAll('#ls-steps-rows tr'),"
                      "function(r){return r.innerText;}).join('|')}")

    def click_row(prefix):
        script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
               "for (var i=0;i<rows.length;i++){ if (rows[i].getAttribute('data-study').indexOf('%s')===0){rows[i].click();return;} }" % prefix)

    try:
        rq("POST", base + "/url", {"url": UI})
        time.sleep(1.5)
        script("document.querySelector('.nav-item[data-tab=\"live-study\"]').click()")
        time.sleep(1.5)

        # 1. a slow read of study A, study B opened before it returns
        delay_record(DONE, 1, 3500)
        click_row("bb")
        time.sleep(0.4)
        click_row("ee")
        time.sleep(1.5)
        mid = on_screen()
        time.sleep(4.5)
        end = on_screen()
        print("info  mid-flight:", mid)
        check("A's late read renders nothing under B (steps, name, id)",
              BARE in end["id"] and end["name"] == "no-taps-at-all"
              and "noop" in end["steps"] and "hrm-start" not in end["steps"]
              and "ble-connect" not in end["steps"], end)

        # 2. the post-run re-read of a study that just finished, then B opened
        delay_record(RUNNING, 2, 5000)
        click_row("aa")
        for _ in range(60):
            if script("return window.__seen") >= 2: break
            time.sleep(0.25)
        check("the post-run re-read started", script("return window.__seen") >= 2)
        click_row("ee")
        time.sleep(1.5)
        time.sleep(5.5)
        end = on_screen()
        check("the post-run re-read of A renders nothing under B",
              BARE in end["id"] and end["name"] == "no-taps-at-all" and "noop" in end["steps"]
              and "hrm-start" not in end["steps"]
              and end["mode"] == "read from disk", end)
        errs = script("return window.__errs || []")
        check("no uncaught JavaScript error", not errs, errs)
    finally:
        try: rq("DELETE", base)
        except Exception: pass
    if fails:
        print(str(len(fails)) + " check(s) FAILED: " + ", ".join(fails)); sys.exit(1)
    print("every check passed")


main()
