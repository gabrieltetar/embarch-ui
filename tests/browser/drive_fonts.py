"""IBM Plex, served by this binary, driven in a real browser.

The third sibling of `drive.py`/`drive_build.py`, and a separate script for
the same reason they are separate from each other: it needs no stub, no
study and no firmware repo — just the UI — and what it checks is a property
of every page rather than of one tab.

**Why it exists.** The fonts used to come from fonts.googleapis.com. That
link was the one asset in the app that could fail *silently*: on a bench
with no route to Google the page still rendered, in Segoe UI and a generic
monospace, with nothing in any log. `tests/fonts.rs` guards the names, which
is all a text guard can do. It cannot tell whether a browser handed the
woff2 actually paints — nor whether `app.js`'s "~6.6 px per character at
11.5 px IBM Plex Mono" is still true of the file being shipped, which is the
measurement the trace view's lane gutter is sized from and the thing that
quietly broke whenever the CDN was unreachable.

Run it like the others:

    cargo build --release
    geckodriver --port 4444 &
    EMBARCH_UI_PORT=4899 ./target/release/embarch-ui &
    python3 tests/browser/drive_fonts.py
"""
import json, os, urllib.request, urllib.error, time, sys

UI = "http://127.0.0.1:" + os.environ.get("EMBARCH_UI_PORT", "4899")
BASE = "http://127.0.0.1:4444"
# The constant in `assets/app.js` (`traceGutter`), and the size it was
# measured at. Kept here as two numbers rather than scraped out of the JS:
# if somebody re-measures the gutter, this file should be the second place
# they have to change, not a mirror that silently follows. It was 6.6 in
# app.js until this check was written and measured the shipped font at 6.9
# — a 4.5% under-measurement that nothing could have caught while the font
# came from a CDN that might not answer.
GUTTER_PX_PER_CHAR = 6.9
GUTTER_FONT_PX = 11.5

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

# --- the routes, before any browser is involved --------------------------
for name in ["ibm-plex-sans-var-latin.woff2", "ibm-plex-mono-400-latin.woff2",
             "ibm-plex-mono-600-latin.woff2"]:
    with urllib.request.urlopen(UI + "/fonts/" + name, timeout=30) as r:
        body = r.read()
        check("GET /fonts/" + name,
              r.status == 200 and body[:4] == b"wOF2"
              and r.headers.get("content-type") == "font/woff2"
              and "immutable" in (r.headers.get("cache-control") or ""),
              str(len(body)) + " bytes")
try:
    urllib.request.urlopen(UI + "/fonts/not-a-font.woff2", timeout=30)
    check("an unknown font name 404s", False, "it returned 200")
except urllib.error.HTTPError as e:
    check("an unknown font name 404s", e.code == 404, e.code)

sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
    "moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
b = BASE + "/session/" + sid
def script(js, args=None):
    return rq("POST", b + "/execute/sync", {"script": js, "args": args or []})
def script_async(js, args=None):
    """WebDriver's async form: the script is handed a callback as its last
    argument and the call returns whatever it passes. Needed here because
    every font question worth asking is a promise."""
    return rq("POST", b + "/execute/async", {"script": js, "args": args or []})
try:
    rq("POST", b + "/url", {"url": UI + "/"})
    time.sleep(2)
    # `document.fonts.ready` rather than a sleep: the one signal that says
    # the faces the page asked for are resolved, one way or the other.
    check("document.fonts.ready settles",
          script_async("var d=arguments[0];"
                       "document.fonts.ready.then(function(){d(true)});") is True)

    # `document.fonts.check` answers "is it ready to paint *now*", and a
    # face nothing on the current view uses has not been fetched yet — so
    # each one is asked for first. `load` rejecting, or resolving to an
    # empty list, is the real failure: that is a face the browser could not
    # get.
    for face in ['400 14px "IBM Plex Sans"', '650 14px "IBM Plex Sans"',
                 '700 14px "IBM Plex Sans"', '400 12px "IBM Plex Mono"',
                 '600 12px "IBM Plex Mono"']:
        got = script_async(
            "var d=arguments[1];document.fonts.load(arguments[0])"
            ".then(function(fs){d(fs.length)},function(){d(-1)});",
            [face])
        check("the browser can paint " + face,
              got > 0 and script("return document.fonts.check(%s);" % json.dumps(face)),
              "%s face(s) matched" % got)

    loaded = script("return Array.from(document.fonts).map(function(f){"
                    "return f.family+' '+f.weight+' '+f.status;});")
    check("every declared face loaded, none errored",
          all(f.endswith("loaded") for f in loaded), loaded)

    # Nothing reached the network for a font. `performance` sees every
    # subresource the page fetched, so this is the check the text guard in
    # `tests/fonts.rs` cannot make: not "the source has no CDN link" but
    # "this render did not touch one".
    res = script("return performance.getEntriesByType('resource')"
                 ".map(function(e){return e.name;});")
    check("no request went to a font CDN",
          not any("fonts.googleapis.com" in n or "fonts.gstatic.com" in n for n in res))
    check("the fonts came from this server",
          sum(1 for n in res if "/fonts/" in n) >= 2,
          [n for n in res if "/fonts/" in n])

    # The measurement `app.js` sizes the trace gutter from, taken against
    # the font that actually painted. A fallback monospace lands well
    # outside this band, which is exactly the failure the CDN link used to
    # cause without a word.
    w = script(
        "var c=document.createElement('canvas').getContext('2d');"
        "c.font='%spx \"IBM Plex Mono\"';"
        "return c.measureText('MMMMMMMMMMMMMMMMMMMM').width/20;" % GUTTER_FONT_PX)
    check("Plex Mono advance matches app.js's %.1f px/char" % GUTTER_PX_PER_CHAR,
          abs(w - GUTTER_PX_PER_CHAR) < 0.2, "%.3f px/char measured" % w)

    # And that the shell is actually wearing it, not just that it loaded.
    fam = script("return getComputedStyle(document.querySelector('.word'))"
                 ".fontFamily;")
    check("the wordmark computes to Plex Sans", "IBM Plex Sans" in fam, fam)
finally:
    rq("DELETE", b)

print()
print(("FAILED: " + ", ".join(fails)) if fails else "all checks passed")
sys.exit(1 if fails else 0)
