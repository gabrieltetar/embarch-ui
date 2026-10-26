#!/usr/bin/env python3
"""After a project switch the Live Study picker lists only the new project's
saved studies, with no other tab visited first (ui/083).

Needs stub_core.py, a UI started with XDG_DATA_HOME pointed at a scratch dir
(the recents list lives there), geckodriver, and two scratch repos passed as
argv: each a directory with `.git/` and `embarch/studies/<slug>.json`."""
import json, sys, time, urllib.request

GECKO = "http://127.0.0.1:4444"
UI = "http://127.0.0.1:4899"
REPO_A, REPO_B = sys.argv[1], sys.argv[2]


def rq(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(GECKO + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read() or b"{}")


def main():
    sid = rq("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "firefox", "moz:firefoxOptions": {"args": ["-headless"]}}}})["value"]["sessionId"]
    base = "/session/" + sid
    script = lambda js: rq("POST", base + "/execute/sync", {"script": js, "args": []})["value"]
    fails = []
    def check(name, ok, detail=""):
        print(("PASS  " if ok else "FAIL  ") + name + (("  — " + str(detail)) if detail else ""))
        if not ok: fails.append(name)
    picker = "return Array.prototype.map.call(document.querySelectorAll('#ls-study-picker option'),function(o){return o.value;}).join(',')"
    try:
        for repo in (REPO_A, REPO_B):
            urllib.request.urlopen(urllib.request.Request(
                UI + "/api/study-designer/project", method="POST",
                data=json.dumps({"path": repo}).encode(),
                headers={"Content-Type": "application/json"}), timeout=10)
            break
        rq("POST", base + "/url", {"url": UI})
        time.sleep(2)
        check("the picker lists project A's study at load", script(picker) == "alpha-old", script(picker))
        # Switch through the UI's own open-project path (no tab change).
        script("document.getElementById('project-path').value=%s;"
               "document.getElementById('project-open').click();" % json.dumps(REPO_B))
        time.sleep(2.5)
        check("after the switch the picker lists only project B's study",
              script(picker) == "beta-new", script(picker))
    finally:
        try: rq("DELETE", base)
        except Exception: pass
    if fails:
        print(str(len(fails)) + " check(s) FAILED: " + ", ".join(fails)); sys.exit(1)
    print("every check passed")


main()
