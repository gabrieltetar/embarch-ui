"""The Build card and the Debug tab's `builds` source, in a real browser.

`drive.py`'s sibling, and deliberately a second script rather than more
checks inside it: that one drives the Live Study tab against `stub_core.py`'s
fixtures, and these two surfaces need neither a stub nor a study. What they
do need is the **unavailable** path, which is the one a fresh checkout
actually has — no firmware repo open, no `embarch-api` config, no build ever
run — and which is where the card has to say why rather than show a dead
toggle or sit on a placeholder that reads as a hung request.

The available path is not driven here. It needs a configured project and a
west workspace, so it is checked on the bench (see
`embarch-doc/embarch-ui/spec.md`), not in a script anybody can run.

Run it the same way as `drive.py`:

    cargo build --release
    geckodriver --port 4444 &
    EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
    python3 tests/browser/drive_build.py
"""
import json, urllib.request, time, sys
BASE = "http://127.0.0.1:4444"
def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]
fails = []
def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond: fails.append(name)

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
    "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid
def script(js, args=None):
    return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})
try:
    rq("POST", b + "/url", {"url": "http://127.0.0.1:4899/"})
    script("window.__errs=[];window.addEventListener('error',function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)

    # --- the Build card ---------------------------------------------------
    script("document.querySelector('.nav-item[data-tab=\"study-designer\"]').click();")
    time.sleep(3)
    check("the Build card exists", script("return !!document.getElementById('sd-build-card');"))
    note = script("return document.getElementById('sd-build-note').textContent;")
    check("an unbuildable bench says why, rather than showing a dead toggle",
          "cannot" in note or "not" in note or "no " in note, note[:120])
    check("the toggle is disabled when nothing can be built",
          script("return document.getElementById('sd-build-on').disabled === true;"))
    check("the body stays hidden while the toggle is off",
          script("return document.getElementById('sd-build-body').style.display === 'none';"))

    # --- the Debug tab's builds source ------------------------------------
    script("document.querySelector('.nav-item[data-tab=\"debug\"]').click();")
    time.sleep(1)
    check("the builds chip exists",
          script("return !!document.querySelector('.chip[data-log-source=\"builds\"]');"))
    check("the build picker is hidden while another source is selected",
          script("return document.getElementById('log-build-pick').style.display === 'none';"))
    script("document.querySelector('.chip[data-log-source=\"builds\"]').click();")
    time.sleep(2)
    check("switching to builds shows the picker",
          script("return document.getElementById('log-build-pick').style.display !== 'none';"))
    check("the subtitle follows the source",
          "build" in script("return document.getElementById('debug-subtitle').textContent;").lower(),
          script("return document.getElementById('debug-subtitle').textContent;"))
    check("an empty build directory reads as 'none yet', not as an error",
          script("return document.getElementById('log-build-pick').textContent;").strip() == "no builds yet",
          script("return document.getElementById('log-build-pick').textContent;"))
    check("no error card is shown for a bench that has never built",
          script("return document.getElementById('debug-error').style.display === 'none';"))
    script("document.querySelector('.chip[data-log-source=\"core\"]').click();")
    time.sleep(1)
    check("switching back hides the picker again",
          script("return document.getElementById('log-build-pick').style.display === 'none';"))

    errs = script("return window.__errs;")
    check("no uncaught JavaScript error", not errs, errs)
finally:
    rq("DELETE", b, None)
print(("\n%d FAILED" % len(fails)) if fails else "\nall passed")
sys.exit(1 if fails else 0)
