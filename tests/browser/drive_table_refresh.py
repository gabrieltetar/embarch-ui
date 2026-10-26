#!/usr/bin/env python3
"""A running study's Data card ends on the full table, not the mid-run
snapshot the card first cached (ui/085, embarch-ui decision 64).

stub_core.py serves the running study's `rail` capture at 3 rows until its run
ends, then 40. Opening the study loads the card at 3; the post-run re-read must
bring it to 40. Needs stub_core.py, a UI pointed at it, geckodriver:

  python3 tests/browser/stub_core.py <core port> &
  geckodriver --port <gecko port> &
  EMBARCH_UI_CONFIG=<toml> EMBARCH_UI_PORT=<ui port> ./target/debug/embarch-ui &
  GECKO=http://127.0.0.1:<gecko port> UI=http://127.0.0.1:<ui port> \\
    python3 tests/browser/drive_table_refresh.py
"""
import json, os, sys, time, urllib.request

GECKO = os.environ.get("GECKO", "http://127.0.0.1:4444")
UI = os.environ.get("UI", "http://127.0.0.1:4899")


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

    def rail_note():
        return script("var e=document.getElementById('ls-data-rail-note');return e?e.textContent:''")

    fails = []
    try:
        rq("POST", base + "/url", {"url": UI})
        time.sleep(1.5)
        script("document.querySelector('.nav-item[data-tab=\"live-study\"]').click()")
        time.sleep(1.5)
        script("var rows=document.querySelectorAll('#ls-studies-rows tr');"
               "for (var i=0;i<rows.length;i++){ if (rows[i].getAttribute('data-study').indexOf('aaaa')===0){rows[i].click();return;} }")
        first = ""
        for _ in range(40):
            first = rail_note()
            if "of 3 row" in first: break
            time.sleep(0.25)
        print("info  while running:", first)
        ok1 = "of 3 row" in first
        print(("PASS  " if ok1 else "FAIL  ") + "the card first loads the mid-run table (3 rows)")
        if not ok1: fails.append("first load")
        last = ""
        for _ in range(60):
            last = rail_note()
            if "of 40 row" in last: break
            time.sleep(0.25)
        print("info  after the run:", last)
        ok2 = "showing 40 of 40 row" in last
        print(("PASS  " if ok2 else "FAIL  ") + "the post-run re-read shows the full table (40 rows)")
        if not ok2: fails.append("post-run")
    finally:
        try: rq("DELETE", base)
        except Exception: pass
    if fails:
        print(str(len(fails)) + " check(s) FAILED: " + ", ".join(fails)); sys.exit(1)
    print("every check passed")


main()
