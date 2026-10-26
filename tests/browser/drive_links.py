"""Links into the page that name what to show, in a real browser.

`embarch-ui` decision 58: a fragment may carry parameters, `#<tab>?<query>`,
and the chat panel's step links use three of them — `#desk?item=<work item>`,
`#live-study?study=<study id>` and `#topology?board=<role or board type>`.
Each is driven twice: as the address the page is loaded with, and as a
`hashchange` while the page is already open (a link followed into a browser
tab that already shows the UI fires nothing else). Plus what must not
change — a plain `#tab`, a retired one — and what must never happen: a
parameter becoming markup.

Brings everything it needs on its own ports: a stub embarch-core (the
Live Study fixtures of `stub_core.py`, plus two enrolled roles), a scratch
embarch-desk with scratch config and data directories (the real desk, its
tokens and its state are never touched), a scratch firmware repo with one
board type in its catalog, and an embarch-ui whose recent-projects list and
data directory are scratch too.

    cargo build && (cd ../embarch-desk && cargo build)
    geckodriver --port 4444 &
    python3 tests/browser/drive_links.py
"""
import json, os, shutil, subprocess, sys, tempfile, threading, time, urllib.parse, urllib.request
from http.server import ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

sys.dont_write_bytecode = True   # no __pycache__ left in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))
import stub_core  # noqa: E402  (the Live Study fixtures, reused rather than copied)
import addr

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
DESK_BIN = os.environ.get("EMBARCH_DESK_BIN", str(REPO.parent / "embarch-desk" / "target" / "debug" / "embarch-desk"))
UI_PORT = addr.ui_port(4895)
DESK_PORT = os.environ.get("LINKS_DESK_PORT", "4997")
CORE_PORT = int(os.environ.get("LINKS_CORE_PORT", "4904"))
UI = "http://127.0.0.1:" + UI_PORT
DESK = "http://127.0.0.1:" + DESK_PORT
BASE = addr.gecko_url()

ENROLLED = [
    {"probe_serial": "000683001234", "role": "dev-bench", "name": "esp32c5_devkitc/esp32c5/hpcore",
     "chip": "esp32c5", "hardware_id": "aa:bb", "confirmed_at_utc_ms": 1758240000000, "link_serial": None},
    {"probe_serial": "ABC123", "role": "dut", "name": "nrf54l15dk",
     "chip": "nRF54L15", "hardware_id": "cc:dd", "confirmed_at_utc_ms": 1758240000000, "link_serial": None},
]


class Core(stub_core.Handler):
    """`stub_core.py`'s studies, on a bench with both roles enrolled."""

    def do_GET(self):
        if self.headers.get("authorization") == "Bearer " + stub_core.TOKEN:
            path = urlparse(self.path).path
            if path == "/probes/enrolled":
                return self._json(ENROLLED)
            if path == "/serial-ports":
                return self._json([])
        return super().do_GET()


fails = []


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


def wd(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read().decode())["value"]
    except urllib.error.HTTPError as e:
        raise RuntimeError("webdriver: " + e.read().decode()[:600]) from None


def http(method, url, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    with urllib.request.urlopen(urllib.request.Request(url, data=data, method=method, headers=headers), timeout=30) as r:
        text = r.read().decode()
    try:
        return json.loads(text or "{}")
    except ValueError:
        return text


def wait_for(url, secs=20):
    for _ in range(secs * 10):
        try:
            urllib.request.urlopen(url, timeout=1)
            return True
        except Exception:
            time.sleep(0.1)
    return False


def main():
    scratch = Path(tempfile.mkdtemp(prefix="drive-links-"))
    srv = ThreadingHTTPServer(("127.0.0.1", CORE_PORT), Core)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    env = {**os.environ, "EMBARCH_DESK_CONFIG_DIR": str(scratch / "cfg"), "EMBARCH_DESK_DATA_DIR": str(scratch / "data"),
           "EMBARCH_DESK_PORT": DESK_PORT, "CLAUDE_CONFIG_DIR": str(scratch / "claude"),
           "EMBARCH_UI_STATE": str(scratch / "ui-recent-projects.json"), "XDG_DATA_HOME": str(scratch / "xdg-data")}
    cfg = scratch / "ui.toml"
    cfg.write_text('[core]\nbase_url = "http://127.0.0.1:%d"\ntoken = "%s"\n' % (CORE_PORT, stub_core.TOKEN))
    # A firmware repo whose catalog lists one board type no role holds.
    fw = scratch / "fw"
    (fw / ".git").mkdir(parents=True)
    (fw / "embarch").mkdir()
    (fw / "embarch" / "boards.toml").write_text('[[boards]]\nname = "plank"\nchip = "nRF54L15"\nbuild_target = "plank/nrf54l15/cpuapp"\n')
    procs = []
    try:
        procs.append(subprocess.Popen([DESK_BIN, "serve"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        check("the scratch desk starts", wait_for(DESK + "/health"))
        token = (scratch / "cfg" / "desk-token").read_text().strip()
        # Eight cards, newest first, so the oldest is well below the fold.
        items = [http("POST", DESK + "/items", {"title": "Card %d" % i}, token)["id"] for i in range(8)]
        oldest, second = items[0], items[1]
        gone = http("POST", DESK + "/items", {"title": "Archived card"}, token)["id"]
        http("POST", DESK + "/items/%s/archive" % gone, {"value": True}, token)

        procs.append(subprocess.Popen([BIN], env={**env, "EMBARCH_UI_CONFIG": str(cfg), "EMBARCH_UI_PORT": UI_PORT},
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        check("the scratch UI starts", wait_for(UI + "/"))

        sid = wd("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
        S = BASE + "/session/" + sid
        try:
            def js(src, *args):
                return wd("POST", S + "/execute/sync", {"script": src, "args": list(args)})

            def until(src, secs=15, *args):
                v = None
                for _ in range(secs * 5):
                    v = js(src, *args)
                    if v:
                        return v
                    time.sleep(0.2)
                return v

            def go(hash_):
                # A load, not a hop: navigating to the page already open with
                # only a new fragment is a fragment navigation, no load at all.
                wd("POST", S + "/url", {"url": "about:blank"})
                wd("POST", S + "/url", {"url": UI + "/" + hash_})
                check("(a fresh page for " + hash_[:40] + ")", js("return performance.getEntriesByType('navigation').length === 1 && !window.__loaded && (window.__loaded = true)") is True)

            def hop(hash_):
                # A link followed into the page already open: only the fragment changes.
                js("location.hash = arguments[0]; return true", hash_)

            def active():
                return js("var p = document.querySelector('.tab-panel.active'); return p && p.dataset.tab")

            def note():
                return js("var n = document.getElementById('route-note'); return n.hidden ? null : n.textContent")

            def marked(css):
                return js("return Array.from(document.querySelectorAll(arguments[0] + '.link-focus'))"
                          ".map(function (e) { return e.getAttribute('data-card') || e.getAttribute('data-enroll-role') ||"
                          " (e.querySelector('[data-board-edit]') || {getAttribute: function () { return null; }}).getAttribute('data-board-edit'); })", css)

            def in_view(css_or_el_js):
                return js("var e = " + css_or_el_js + "; if (!e) return null; var r = e.getBoundingClientRect(),"
                          " c = document.querySelector('.content').getBoundingClientRect();"
                          " return r.top >= c.top - 1 && r.bottom <= c.bottom + 1")

            def real_click(css):
                el = wd("POST", S + "/element", {"using": "css selector", "value": css})
                wd("POST", S + "/element/" + list(el.values())[0] + "/click", {})

            card = "document.querySelector('.desk-card[data-card=\"' + %s + '\"]')"
            wd("POST", S + "/window/rect", {"width": 1200, "height": 700})

            # ---- a plain #tab is what it was ---------------------------------
            go("#topology")
            check("plain #topology lands on Topology, as before", until("return document.querySelector('.tab-panel.active[data-tab=topology]') && true") is True)
            time.sleep(1.5)
            check("and marks nothing, says nothing, and keeps its address",
                  marked("[data-enroll-role]") == [] and note() is None and js("return location.hash") == "#topology",
                  [marked("[data-enroll-role]"), note(), js("return location.hash")])
            hop("#live-study")
            check("a plain #live-study while open switches the tab", until("return document.querySelector('.tab-panel.active[data-tab=\"live-study\"]') && true") is True)
            hop("#enroll")
            check("a retired #enroll still lands on Topology", until("return document.querySelector('.tab-panel.active[data-tab=topology]') && true") is True)
            hop("#dashboard")
            check("a retired #dashboard still lands on the Desk", until("return document.querySelector('.tab-panel.active[data-tab=desk]') && true") is True)

            # ---- #desk?item= -------------------------------------------------
            go("#desk?item=" + urllib.parse.quote(oldest))
            got = until("return document.querySelectorAll('.desk-card').length >= 8 && document.querySelector('.desk-card.link-focus') && true")
            check("#desk?item= on load: the Desk shows and that card, and only it, is marked",
                  got is True and active() == "desk" and marked(".desk-card") == [oldest], marked(".desk-card"))
            check("the card was scrolled to: it started below the fold and is in view now",
                  until("return document.querySelector('.content').scrollTop > 0") is True and in_view(card % json.dumps(oldest)) is True,
                  js("return document.querySelector('.content').scrollTop"))
            check("the parameters left the address, the tab stayed", js("return location.hash") == "#desk", js("return location.hash"))
            check("no notice for a card that is there", note() is None, note())
            # A pushed view rebuilds every card; the mark survives it.
            js("document.querySelector('.desk-card[data-card=\"' + arguments[0] + '\"]').setAttribute('data-old', '1'); return true", oldest)
            http("POST", DESK + "/items", {"title": "Pushed after the link"}, token)
            rebuilt = until("return document.getElementById('desk-cards').textContent.indexOf('Pushed after the link') >= 0", 25)
            check("a pushed view rebuilds the cards and the card stays marked",
                  rebuilt is True and js("return !document.querySelector('[data-old]')") and marked(".desk-card") == [oldest], marked(".desk-card"))
            hop("#desk?item=" + urllib.parse.quote(second))
            check("#desk?item= while open: the other card is marked instead",
                  until("return document.querySelector('.desk-card.link-focus[data-card=\"' + arguments[0] + '\"]') && true", 10, second) is True
                  and marked(".desk-card") == [second], marked(".desk-card"))
            hop("#desk?item=" + urllib.parse.quote(oldest))
            check("and the first link followed again works (its parameters had left the address)",
                  until("return document.querySelector('.desk-card.link-focus[data-card=\"' + arguments[0] + '\"]') && true", 10, oldest) is True)
            real_click(".topbar-title")
            check("the reader's first click clears the mark", until("return !document.querySelector('.link-focus')", 5) is True)
            hop("#desk?item=" + urllib.parse.quote(gone))
            n = until("var n = document.getElementById('route-note'); return !n.hidden && n.textContent", 10)
            check("an archived card: the Desk as it is, nothing marked, and a one-line notice naming it",
                  bool(n) and gone in n and active() == "desk" and marked(".desk-card") == [], n)
            real_click(".topbar-title")
            check("which the next click clears", until("return document.getElementById('route-note').hidden", 5) is True)
            evil = '<img src=x onerror="window.__pwned=1">'
            go("#desk?item=" + urllib.parse.quote(evil))
            n = until("var n = document.getElementById('route-note'); return !n.hidden && n.textContent", 15)
            check("an item id is text, never markup", bool(n) and evil in n
                  and js("return !document.querySelector('#route-note img, .desk-cards img') && !window.__pwned"), n)

            # ---- #live-study?study= -----------------------------------------
            go("#live-study?study=" + stub_core.DONE)
            got = until("var e = document.getElementById('ls-status-id'); return e && e.textContent")
            check("#live-study?study= on load: a past study opens on Live Study, read off disk",
                  got == stub_core.DONE and active() == "live-study"
                  and until("return document.getElementById('ls-steps-rows').children.length === 2") is True,
                  [got, active()])
            check("its id is in the Open box, and the status card is in view",
                  js("return document.getElementById('ls-open-id').value") == stub_core.DONE
                  and in_view("document.getElementById('ls-status-id')") is True)
            check("no notice for a study Core has", note() is None, note())
            hop("#live-study?study=" + stub_core.RUNNING)
            got = until("var e = document.getElementById('ls-status-id'); return e && e.textContent === arguments[0] && e.textContent", 10, stub_core.RUNNING)
            check("#live-study?study= while open: the running study replaces it", got == stub_core.RUNNING, got)
            check("and it is watched live: its console fills from the stream",
                  until("return document.getElementById('ls-consoles').textContent.indexOf('booted') >= 0", 10) is True)
            unknown = "ff" * 16
            hop("#live-study?study=" + unknown)
            n = until("var n = document.getElementById('route-note'); return !n.hidden && n.textContent", 10)
            check("a study Core has never heard of: the tab, its own refusal, and a notice naming it", bool(n) and unknown in n, n)

            # ---- #topology?board= -------------------------------------------
            go("#topology?board=dut")
            got = until("return document.querySelector('[data-enroll-role].link-focus') && true")
            check("#topology?board=dut on load: the DUT box, and only it, is marked",
                  got is True and active() == "topology" and marked("[data-enroll-role]") == ["dut"], marked("[data-enroll-role]"))
            js("document.querySelector('#topology-diagram > *').setAttribute('data-old', '1'); return true")
            redrawn = until("return !document.querySelector('#topology-diagram [data-old]')", 12)
            check("the next snapshot redraws the diagram and the box stays marked",
                  redrawn is True and marked("[data-enroll-role]") == ["dut"], marked("[data-enroll-role]"))
            for label, role in (("DUT", "dut"), ("Dev bench", "dev-bench"), ("nrf54l15dk", "dut"),
                                ("esp32c5_devkitc", "dev-bench"), ("esp32c5_devkitc/esp32c5/hpcore", "dev-bench")):
                hop("#topology?board=" + urllib.parse.quote(label))
                got = until("var m = document.querySelectorAll('[data-enroll-role].link-focus');"
                            " return m.length === 1 && m[0].getAttribute('data-enroll-role') === arguments[0]", 10, role)
                check("#topology?board=%s while open marks the %s box" % (label, role), got is True, marked("[data-enroll-role]"))
            hop("#topology?board=plank")
            n = until("var n = document.getElementById('route-note'); return !n.hidden && n.textContent", 10)
            check("a board no role holds and no catalog lists: nothing marked, a notice naming it",
                  bool(n) and "plank" in n and marked("[data-enroll-role]") == [], n)
            http("POST", UI + "/api/study-designer/project", {"path": str(fw)})
            go("#topology?board=plank")
            got = until("return document.querySelector('#board-catalog-body tr.link-focus') && true", 15)
            check("with a project open, a board type only its catalog lists marks the catalog row",
                  got is True and marked("#board-catalog-body tr") == ["plank"] and marked("[data-enroll-role]") == [] and note() is None,
                  [marked("#board-catalog-body tr"), note()])
            hop("#topology?board=" + urllib.parse.quote(evil))
            n = until("var n = document.getElementById('route-note'); return !n.hidden && n.textContent", 10)
            check("a board name is text, never markup", bool(n) and evil in n
                  and js("return !document.querySelector('#route-note img') && !window.__pwned"), n)
            hop("#topology?board=dut")
            until("return document.querySelector('[data-enroll-role].link-focus') && true", 10)
            hop("#desk")
            check("a plain #tab followed later drops the earlier link's mark and notice",
                  until("return document.querySelector('.tab-panel.active[data-tab=desk]') && !document.querySelector('.link-focus')"
                        " && document.getElementById('route-note').hidden", 10) is True, [marked("[data-enroll-role]"), note()])
        finally:
            wd("DELETE", S)
    finally:
        for p in procs:
            if p.poll() is None:
                p.terminate()
                p.wait(5)
        srv.shutdown()
        shutil.rmtree(scratch, ignore_errors=True)

    print("\n%d failed" % len(fails) if fails else "\nall passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
