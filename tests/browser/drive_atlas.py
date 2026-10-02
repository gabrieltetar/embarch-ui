"""The Atlas tab, in a real browser, against a synthetic atlas.

Brings its own project: a throwaway git repo holding one source file, and an
`embarch/atlas/` with one atlas whose `graph.json` is the fixture in
`tests/fixtures/atlas/` — the two-board demo product of embarch-atlas's own
tests (an MCU board and a sensor board behind a mirrored connector), nothing
from a real product. The atlas is pinned to the repo's commit, so the code
citation route has something real to show, and a one-page PDF stands in for
every source document so the page tier renders.

It starts its own embarch-ui (no Core needed: the tab makes no Core call) and
drives what `cargo test` cannot see — that every part is drawn as a symbol,
that a box shows pin names rather than numbers, that a pin's net label
carries the firmware's name for it, that selecting a part draws its wires,
that a net and a problem open in the inspector with their citations.

    cargo build --release
    geckodriver --port 4444 &
    python3 tests/browser/drive_atlas.py
"""
import json, os, shutil, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "release" / "embarch-ui"))
PORT = os.environ.get("ATLAS_UI_PORT", "4897")
UI = "http://127.0.0.1:" + PORT
BASE = "http://127.0.0.1:4444"
FIXTURE = REPO / "tests" / "fixtures" / "atlas" / "graph.json"


def rq(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())["value"]


fails = []


def check(name, cond, extra=""):
    print(("PASS  " if cond else "FAIL  ") + name + (("  — " + str(extra)) if extra else ""))
    if not cond:
        fails.append(name)


def one_page_pdf(path: Path) -> None:
    """A valid single-page PDF with a correct xref, small enough to write by hand."""
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
        b"/Resources << /Font << /F1 5 0 R >> >> >>",
        None,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    stream = b"BT /F1 24 Tf 72 700 Td (synthetic page) Tj ET"
    objs[3] = b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream"
    out = bytearray(b"%PDF-1.4\n")
    offs = []
    for i, o in enumerate(objs, 1):
        offs.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    for o in offs:
        out += b"%010d 00000 n \n" % o
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
    path.write_bytes(bytes(out))


def project(tmp: Path) -> tuple[Path, str]:
    fw = tmp / "demo-fw"
    fw.mkdir()
    (fw / "board.dts").write_text("/ {\n\tled: led_0 { gpios = <&gpioa 5 GPIO_ACTIVE_LOW>; };\n};\n" + "\n" * 40)
    git = lambda *a: subprocess.run(["git", "-C", str(fw), *a], check=True, capture_output=True, text=True).stdout.strip()
    git("init", "-q")
    git("add", "-A")
    git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "demo")
    head = git("rev-parse", "HEAD")
    aid = "demo@" + head[:12]
    ad = fw / "embarch" / "atlas"
    atl = ad / "atlases" / aid
    atl.mkdir(parents=True)
    g = json.loads(FIXTURE.read_text())
    g["atlas"], g["commit"] = aid, head
    pdf = tmp / "doc.pdf"
    one_page_pdf(pdf)
    for d in g["docs"].values():
        d["pdf"] = str(pdf)
    (atl / "graph.json").write_text(json.dumps(g))
    (atl / "atlas.json").write_text(json.dumps({"id": aid, "target": "demo", "board": "demo_board",
                                                "commit": head, "created": "2026-01-01T00:00:00Z", "build": {}}))
    for d, meta in g["docs"].items():
        if meta.get("card"):
            (ad / "docs" / d).mkdir(parents=True, exist_ok=True)
            (ad / "docs" / d / "card.md").write_text("# synthetic card\n\nA0 strap selects the address (p.1).\n")
    return fw, aid


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="atlas-drive-"))
    fw, aid = project(tmp)
    cfg = tmp / "ui.toml"
    cfg.write_text('[core]\nbase_url = "http://127.0.0.1:1"\ntoken = "x"\n'
                   f'[study_designer]\nfirmware_repo_path = "{fw}"\n')
    ui = subprocess.Popen([BIN], env={**os.environ, "EMBARCH_UI_CONFIG": str(cfg), "EMBARCH_UI_PORT": PORT},
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sid = None
    try:
        for _ in range(40):
            try:
                urllib.request.urlopen(UI + "/api/atlas", timeout=2)
                break
            except OSError:
                time.sleep(0.25)
        ix = json.loads(urllib.request.urlopen(UI + "/api/atlas", timeout=10).read())
        check("the index lists the atlas at HEAD and opens on it",
              ix["default"] == aid and ix["atlases"][0]["head"], ix["default"])

        sid = rq("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {
            "moz:firefoxOptions": {"args": ["-headless", "--width=1600", "--height=1000"]}}}})["sessionId"]
        b = BASE + "/session/" + sid
        rq("POST", b + "/window/rect", {"width": 1600, "height": 1000})

        def js(src, args=None):
            return rq("POST", b + "/execute/sync", {"script": src, "args": args or []})

        def wait(src, timeout=15):
            t0 = time.time()
            while time.time() - t0 < timeout:
                v = js(src)
                if v:
                    return v
                time.sleep(0.25)
            return None

        rq("POST", b + "/url", {"url": UI + "/#atlas"})
        st = wait("return window.__atlas && window.__atlas.state().nodes ? window.__atlas.state() : null")
        g = json.loads(FIXTURE.read_text())
        check("the map loads the atlas's graph", st and st["nodes"] == g["counts"]["nodes"], st)
        parts = [n for n in g["nodes"] if n["kind"] == "part"]
        check("every part on both boards is drawn", js("return document.querySelectorAll('#atlas-map .at-part').length") == len(parts))
        check("resistors are zigzag symbols, ICs and connectors boxes",
              js("return document.querySelectorAll('.at-part.sym-resistor .at-sym:not(.box)').length") == sum(1 for n in parts if n["sym_kind"] == "resistor")
              and js("return document.querySelectorAll('.at-part.sym-ic .at-sym.box').length") >= 3)

        # inside a box: the pin's name, not its number
        names = js("return [...document.querySelectorAll('.at-part[data-id=\"hw:M:U3\"] .at-pn')].map(t=>t.textContent)")
        check("an IC box shows pin names (SDA), not package numbers", "SDA" in names and "B3" not in names, names)
        conn = js("return [...document.querySelectorAll('.at-part[data-id=\"hw:M:J1\"] .at-pn')].map(t=>t.textContent)")
        check("a connector box shows pin numbers", "1" in conn, conn)
        # outside: the net, and the firmware's name for it
        lab = js("return [...document.querySelectorAll('.at-part[data-id=\"hw:M:U3\"] .at-nl')].map(t=>t.textContent)")
        check("a pin's label is the net plus the firmware's name", any(t.startswith("BUS0_SDA · PB7") for t in lab), lab)

        js("window.__atlas.center('hw:M:U3', 3)")
        time.sleep(0.4)
        check("close up, labels are drawn", js("return document.getElementById('atlas-map').classList.contains('lod-near')"))
        js("window.__atlas.zoom(0.05)")
        check("far out, they are not", js("return getComputedStyle(document.querySelector('.at-near')).display") == "none")

        js("window.__atlas.select('hw:M:U3')")
        time.sleep(0.3)
        check("selecting a part draws its wires", js("return document.querySelectorAll('.at-wire').length") > 0)
        check("the inspector names it", js("return document.querySelector('#atlas-inspector .at-title').textContent") == "M:U3")
        check("its chain reaches its driver",
              js("return !!document.querySelector('#atlas-inspector [data-go=\"code:fw:drivers\"]')"))

        js("window.__atlas.selectNet('M:BUS0_SDA', 'hw:M:U3', 0)")
        time.sleep(0.3)
        ins = js("return document.getElementById('atlas-inspector').textContent")
        check("a net opens with its MCU pin, its firmware use and its far side",
              "PB7" in ins and "i2c1_sda" in ins and "S:BUS0_SDA" in ins, ins[:160])

        js("document.getElementById('atlas-p-next').click()")
        time.sleep(0.6)
        check("next walks to the first problem",
              js("return document.getElementById('atlas-p-count').textContent") == "1/" + str(len(g["problems"])))
        # the problem whose evidence cites a firmware line: the excerpt at the atlas's commit
        ix_fw = next(i for i, p in enumerate(g["problems"]) if any((r.get("cite") or {}).get("t") == "code" for r in p["rows"]))
        js("window.__atlas.gotoProblem(arguments[0])", [ix_fw])
        code = wait("var c=document.querySelector('#atlas-inspector .at-code-ex'); return c && c.textContent.indexOf('gpios')>=0 ? c.textContent : null")
        check("a firmware citation shows the cited lines at the atlas's commit", bool(code), code)
        badge = js("var b=document.querySelector('#atlas-inspector .at-code-state'); return b ? b.textContent : ''")
        check("and says the file is unchanged since", badge.startswith("unchanged since"), badge)

        # a schematic page renders, with the part boxed when the export knows where it is
        js("window.__atlas.select('hw:M:U3')")
        time.sleep(0.3)
        js("""var s=[...document.querySelectorAll('#atlas-inspector .at-sec')].filter(x=>x.querySelector('h4')&&x.querySelector('h4').textContent==='Schematic')[0];
              s.querySelector('[data-tier=page]').click();""")
        ok = wait("var i=document.querySelector('#atlas-inspector .at-page img'); return i && i.complete && i.naturalWidth>0")
        check("a cited schematic page renders", bool(ok))

        js("window.__atlas.setTilt(1); window.__atlas.fit(true)")
        time.sleep(0.3)
        check("the 3D view projects every node", js("return window.__atlas.state().T") == 1)
        check("nothing threw", js("return document.getElementById('atlas-empty').style.display") == "none")
    finally:
        if sid:
            try:
                rq("DELETE", BASE + "/session/" + sid)
            except Exception:
                pass
        ui.terminate()
        ui.wait(timeout=10)
        shutil.rmtree(tmp, ignore_errors=True)
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
