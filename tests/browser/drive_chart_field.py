#!/usr/bin/env python3
"""Drives the Study Designer's payload-layout editor: authoring a
`chart_field` (`embarch-study-designer` decision 52's author-time half),
saving it, and confirming it round-trips back into the editor and the
layout chip. Needs an open project (a scratch firmware repo is enough — no
study-structs.toml has to exist yet) but no stub, no study and no hardware."""
import json, sys, time, urllib.request

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
        time.sleep(1.5)
        script("document.querySelector('.nav-item[data-tab=\"study-designer\"]').click()")
        time.sleep(1.5)

        check("no payload layout yet, said plainly",
              script("return document.getElementById('sd-layouts').innerText.indexOf('no payload layout')>=0"))

        # ---- author a new layout with a chart_field ------------------------
        script("document.getElementById('sd-add-layout').click()")
        time.sleep(0.5)
        check("the chart-field picker defaults to none",
              script("return document.getElementById('sd-layout-chart-field').value") == "")
        script("document.getElementById('sd-layout-name').value = 'bds-status'")
        script("document.getElementById('sd-layout-add-header').click()")
        time.sleep(0.2)
        script("var g=document.getElementById('sd-layout-header');"
               "g.querySelector('[data-layout=\"name\"]').value='offset';"
               "g.querySelector('[data-layout=\"name\"]').dispatchEvent(new Event('input',{bubbles:true}));"
               "g.querySelector('[data-layout=\"type\"]').value='u16le';")
        time.sleep(0.2)
        opts = script("return Array.prototype.map.call("
                      "document.getElementById('sd-layout-chart-field').options, function(o){return o.value;})")
        check("typing a field name makes it a chart-field option live",
              "offset" in opts, opts)
        script("document.getElementById('sd-layout-chart-field').value='offset'")
        script("document.getElementById('sd-layout-save').click()")
        time.sleep(0.8)
        result = script("return document.getElementById('sd-layout-result').textContent")
        check("the save succeeds", result == "saved", result)
        time.sleep(0.8)

        # ---- the chip and the raw registry both carry it -------------------
        check("the layout chip shows the chosen chart field",
              script("return document.getElementById('sd-layouts').innerText.indexOf('chart: offset')>=0"))
        raw = json.load(urllib.request.urlopen(UI + "/api/study-designer/structs", timeout=10))
        check("study-structs.toml itself carries chart_field",
              raw.get("struct", [{}])[0].get("chart_field") == "offset", raw)

        # ---- reopening the layout shows the saved choice, not blank --------
        script("document.getElementById('sd-layouts').firstElementChild.click()")
        time.sleep(0.5)
        check("reopening the layout re-selects its saved chart field",
              script("return document.getElementById('sd-layout-chart-field').value") == "offset")

        # ---- clearing it back to none round-trips too -----------------------
        script("document.getElementById('sd-layout-chart-field').value=''")
        script("document.getElementById('sd-layout-save').click()")
        time.sleep(0.8)
        check("un-choosing a chart field saves as none, not left stale",
              script("return document.getElementById('sd-layouts').innerText.indexOf('chart:')") == -1)
        raw2 = json.load(urllib.request.urlopen(UI + "/api/study-designer/structs", timeout=10))
        check("study-structs.toml drops chart_field once cleared",
              "chart_field" not in raw2.get("struct", [{}])[0]
              or raw2["struct"][0]["chart_field"] is None, raw2)

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
