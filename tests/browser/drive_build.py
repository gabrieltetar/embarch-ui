"""The Build card and the Debug tab's `builds` source, in a real browser.

`drive.py`'s sibling, and deliberately a second script rather than more
checks inside it: that one drives the Live Study tab against `stub_core.py`'s
fixtures, and these two surfaces need neither a stub nor a study. What they
do need is the **unavailable** path, which is the one a fresh checkout
actually has — no firmware repo open, no `embarch-api` config, no build ever
run — and which is where the card has to say why rather than show a dead
toggle or sit on a placeholder that reads as a hung request.

**Both paths are driven, and which one runs is decided by asking the server**
rather than by a flag: the survey says whether this bench can build, and the
available half is skipped with a line saying so where it cannot. That keeps
one script correct on a fresh checkout and on a configured bench, instead of
a second script nobody runs on the machine that has the workspace.

Run it the same way as `drive.py`:

    cargo build --release
    geckodriver --port 4444 &
    EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
    python3 tests/browser/drive_build.py
"""
import json, os, urllib.request, time, sys

# `drive.py`'s harness port by default; point it at a real instance (4890)
# to drive the available path against a bench's own workspace.
UI = "http://127.0.0.1:" + os.environ.get("EMBARCH_UI_PORT", "4899")
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
    rq("POST", b + "/url", {"url": UI + "/"})
    script("window.__errs=[];window.addEventListener('error',function(e){window.__errs.push(String(e.message))});")
    time.sleep(2)

    # --- the Build card ---------------------------------------------------
    script("document.querySelector('.nav-item[data-tab=\"study-designer\"]').click();")
    time.sleep(3)
    check("the Build card exists", script("return !!document.getElementById('sd-build-card');"))
    note = script("return document.getElementById('sd-build-note').textContent;")
    check("the card never sits on its loading placeholder",
          "checking what this bench" not in note, note[:120])
    check("the body stays hidden while the toggle is off",
          script("return document.getElementById('sd-build-body').style.display === 'none';"))

    # --- the Debug tab's builds source ------------------------------------
    # --- the Build card, available -----------------------------------------
    #
    # Only where this bench can actually build. The survey is the authority
    # on that, so it is asked rather than guessed at from config.
    with urllib.request.urlopen(UI + "/api/build/survey", timeout=30) as r:
        survey = json.loads(r.read().decode())
    if not survey.get("available"):
        print("SKIP  the available path — this bench has no buildable project open")
    else:
        note = script("return document.getElementById('sd-build-note').textContent;")
        check("the card names the matched project and its config",
              survey["project"] in note and "embarch.toml" in note, note[:110])
        check("the toggle is enabled on a buildable bench",
              script("return document.getElementById('sd-build-on').disabled===false;"))

        script("var t=document.getElementById('sd-build-on');t.checked=true;"
               "t.dispatchEvent(new Event('change'));")
        time.sleep(1)
        check("ticking it reveals the window",
              script("return document.getElementById('sd-build-body').style.display!=='none';"))

        boards = script("return Array.from(document.getElementById('sd-build-board')"
                        ".options).map(o=>o.value);")
        check("the board picker is filled from the live scan", len(boards) > 1 and boards[0] == "",
              boards[:6])
        check("the first option on each axis is the project's own default",
              script("return document.getElementById('sd-build-board')"
                     ".options[0].textContent;") == "(the project's default)")

        # Snippets are per app, so the pool is empty until one is chosen.
        check("the snippet pool asks for an app before offering anything",
              "pick an app" in script("return document.getElementById"
                                      "('sd-build-snippet-pool').textContent;"))
        by_app = (survey.get("targets") or {}).get("snippets_by_app") or {}
        app = next((a for a, ss in by_app.items() if len(ss) >= 2), None)
        if app is None:
            print("SKIP  the ordered snippet list — no app in this repo declares two snippets")
        else:
            script("var a=document.getElementById('sd-build-app');a.value=arguments[0];"
                   "a.dispatchEvent(new Event('change'));", [app])
            time.sleep(1)
            pool = script("return Array.from(document.querySelectorAll"
                          "('#sd-build-snippet-pool .chip')).map(c=>c.textContent.trim());")
            check("choosing an app offers exactly what it declares",
                  len(pool) == len(by_app[app]), f"{len(pool)} vs {len(by_app[app])}")

            first, second = by_app[app][0], by_app[app][1]
            script("var c=Array.from(document.querySelectorAll('#sd-build-snippet-pool .chip'));"
                   "c.find(x=>x.textContent.trim()==='+ '+arguments[0]).click();"
                   "c.find(x=>x.textContent.trim()==='+ '+arguments[1]).click();", [first, second])
            time.sleep(1)
            chosen = script("return Array.from(document.querySelectorAll"
                            "('#sd-build-snippets .chip')).map(c=>c.textContent.trim());")
            check("chosen snippets are numbered in the order they were added",
                  len(chosen) == 2 and chosen[0].startswith("1. " + first)
                  and chosen[1].startswith("2. " + second), chosen)

            # **The whole point of the control.** West applies -S in order and
            # reversals row 109 is the case where that order decides whether
            # the image works, so a picker that cannot reorder is a picker
            # that lies.
            script("document.querySelectorAll('#sd-build-snippets .chip')[1]"
                   ".querySelectorAll('a')[0].click();")
            time.sleep(1)
            after = script("return Array.from(document.querySelectorAll"
                           "('#sd-build-snippets .chip')).map(c=>c.textContent.trim());")
            check("move-up actually reorders, and renumbers",
                  after[0].startswith("1. " + second) and after[1].startswith("2. " + first), after)

        rows = script("return Array.from(document.querySelectorAll"
                      "('#sd-build-flags .req-row .req-label')).map(e=>e.textContent);")
        check("one mode row per served header flag",
              rows == survey["flags"], rows)
        states = script("return Array.from(document.querySelectorAll"
                        "('input[name=\"sd-build-flag-trace_self\"]')).map(r=>r.value);")
        check("each flag offers don't-care / set / clear", states == ["", "set", "clear"], states)
        check("don't care is the default",
              script("return document.querySelector"
                     "('input[name=\"sd-build-flag-trace_self\"]').checked===true;"))

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
