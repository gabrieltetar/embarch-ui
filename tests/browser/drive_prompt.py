#!/usr/bin/env python3
"""The operator-prompt modal, rendered (ui/101, decision 69). Needs stub_core.py
(its prompting host study), a UI pointed at it, geckodriver.

Drives: the modal opens on a tab other than Live Study; the countdown shows and
moves; a resolve from another surface leaves it up saying who answered; a reload
mid-prompt shows it again; Confirm posts `answered_by: "ui"` and closes it; a
stub 409 shows the "already ended" text instead of closing silently.

Run it against a UI started after the stub (the UI's follow of the study is
per process, and the stub's control state is reset at the start and the end)."""
import json, sys, time, urllib.request
import addr

GECKO = addr.gecko_url()
UI = addr.ui_url()
STUB = "http://127.0.0.1:" + __import__("os").environ.get("EMBARCH_STUB_PORT", "4901")
PROMPTING = "a1" * 16


def rq(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(GECKO + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read() or b"{}")


def stub(op, body=None):
    req = urllib.request.Request(STUB + "/_stub/prompt/" + op, data=json.dumps(body or {}).encode(),
                                 method="POST", headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def main():
    for _ in range(50):
        try:
            urllib.request.urlopen(GECKO + "/status", timeout=1); break
        except Exception:
            time.sleep(0.2)
    stub("reset")
    sid = rq("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "firefox", "moz:firefoxOptions": {"args": ["-headless"]}}}})["value"]["sessionId"]
    base = "/session/" + sid

    def script(js):
        return rq("POST", base + "/execute/sync", {"script": js, "args": []})["value"]

    fails = []
    def check(name, ok, detail=""):
        print(("PASS  " if ok else "FAIL  ") + name + (("  - " + str(detail)) if detail else ""))
        if not ok: fails.append(name)

    def modal():
        return script("function t(i){return document.getElementById(i).textContent;}"
                      "function shown(i){var e=document.getElementById(i);return e.style.display!=='none';}"
                      "return {open:shown('prompt-dialog'),step:t('prompt-step'),text:t('prompt-text'),"
                      "count:t('prompt-countdown'),result:t('prompt-result'),resultShown:shown('prompt-result'),"
                      "confirm:shown('prompt-confirm'),cancel:shown('prompt-cancel'),close:shown('prompt-close'),"
                      "tab:(document.querySelector('.tab-panel.active')||{getAttribute:function(){return null}}).getAttribute('data-tab')};")

    def wait_modal(pred, secs=12):
        end = time.time() + secs
        m = modal()
        while time.time() < end:
            m = modal()
            if pred(m): return m
            time.sleep(0.25)
        return m

    try:
        rq("POST", base + "/url", {"url": UI})
        time.sleep(1.5)
        script("document.querySelector('.nav-item[data-tab=\"debug\"]').click()")
        time.sleep(0.5)
        stub("start", {"prompt_id": 1, "text": "Move the probe to J4, then confirm", "deadline_in_ms": 120000})

        # 1. opens on a tab that is not Live Study (the UI polls Core every 3 s)
        m = wait_modal(lambda m: m["open"])
        print("info  modal:", m)
        check("the modal opens on the Debug tab", m["open"] and m["tab"] != "live-study", m)
        check("it shows the step and the prompt text",
              "swap the cable" in m["step"] and m["text"] == "Move the probe to J4, then confirm", m)
        check("Confirm and Cancel are live, Close is not", m["confirm"] and m["cancel"] and not m["close"], m)

        # 2. the countdown shows and moves
        c1 = m["count"]
        time.sleep(2.2)
        c2 = modal()["count"]
        check("the countdown shows and moves", c1.startswith("Time left: ") and c2.startswith("Time left: ") and c1 != c2,
              (c1, c2))

        # 3. a resolve from another surface leaves it up, saying who answered
        stub("resolve", {"prompt_id": 1, "answer": "confirmed", "answered_by": "mcp"})
        m = wait_modal(lambda m: m["resultShown"])
        check("another surface's answer leaves the modal up and names it",
              m["open"] and "confirmed" in m["result"] and "mcp" in m["result"] and m["close"] and not m["confirm"], m)
        script("document.getElementById('prompt-close').click()")
        check("Close dismisses it", not modal()["open"])

        # 4. a reload mid-prompt shows it again
        stub("start", {"prompt_id": 2, "text": "Press the button on the DUT", "deadline_in_ms": 120000})
        m = wait_modal(lambda m: m["open"] and "button" in m["text"])
        check("a second prompt opens", m["open"] and "Press the button" in m["text"], m)
        rq("POST", base + "/refresh", {})
        time.sleep(1.5)
        m = wait_modal(lambda m: m["open"])
        check("a reload mid-prompt shows the modal again",
              m["open"] and "Press the button" in m["text"] and m["count"].startswith("Time left: "), m)

        # 5. Confirm posts answered_by ui and the modal closes
        script("document.getElementById('prompt-confirm').click()")
        m = wait_modal(lambda m: not m["open"])
        answers = stub("answers")["answers"]
        check("Confirm posts the answer as ui", answers and answers[-1].get("answer") == "confirmed"
              and answers[-1].get("prompt_id") == 2 and answers[-1].get("answered_by") == "ui", answers)
        check("the modal closes on its own answer", not m["open"], m)

        # 6. a 409 is shown, not swallowed
        stub("start", {"prompt_id": 3, "text": "Reconnect the battery", "deadline_in_ms": 120000})
        wait_modal(lambda m: m["open"] and "battery" in m["text"])
        stub("conflict", {"on": True})
        script("document.getElementById('prompt-confirm').click()")
        m = wait_modal(lambda m: m["resultShown"])
        check("a 409 says the prompt had already ended and does not close",
              m["open"] and "already ended" in m["result"] and m["close"] and not m["confirm"], m)

        # 7. a host study's prompt shows no run fields and the same buttons (ui/107)
        script("document.getElementById('prompt-close').click()")
        stub("conflict", {"on": False})  # not a reset: the UI's follow reads the stub's frame list by position
        stub("start", {"prompt_id": 4, "text": "Host prompt", "deadline_in_ms": 120000})
        m = wait_modal(lambda m: m["open"] and m["text"] == "Host prompt")
        f = script("return {hidden:document.getElementById('prompt-fields').hidden,"
                   "n:document.querySelectorAll('#prompt-fields input').length,"
                   "label:document.getElementById('prompt-confirm').textContent,"
                   "cancel:document.getElementById('prompt-cancel').style.display}")
        check("a host prompt has no run fields and says Confirm", f["hidden"] and f["n"] == 0
              and f["label"] == "Confirm" and f["cancel"] != "none", f)
        script("document.getElementById('prompt-confirm').click()")
        wait_modal(lambda m: not m["open"])
        sent = stub("answers")["answers"][-1]
        check("a host prompt's answer carries no run_fields key", "run_fields" not in sent, sent)

        # 8. a session Start renders its fields, a refusal keeps the modal open
        stub("start", {"prompt_id": 1, "text": "Place the sensor, then press Start", "deadline_in_ms": 120000,
                       "session": {"phase": "start", "run_fields": [
                           {"name": "serial", "label": "Sensor serial", "required": True},
                           {"name": "room", "label": "Room", "required": False}]}})
        m = wait_modal(lambda m: m["open"] and "Place the sensor" in m["text"])
        f = script("var i=document.querySelectorAll('#prompt-fields input');"
                   "return {n:i.length,names:Array.prototype.map.call(i,function(e){return e.getAttribute('data-name')}),"
                   "labels:Array.prototype.map.call(document.querySelectorAll('.prompt-field-label'),function(e){return e.textContent}),"
                   "label:document.getElementById('prompt-confirm').textContent,"
                   "focus:document.activeElement&&document.activeElement.id}")
        check("a session Start renders one input per declared field, in order",
              f["n"] == 2 and f["names"] == ["serial", "room"] and f["labels"][0].startswith("Sensor serial")
              and f["labels"][0].endswith("*") and f["label"] == "Start" and f["focus"] == "prompt-field-0", f)
        script("document.getElementById('prompt-confirm').click()")
        m = wait_modal(lambda m: m["resultShown"])
        check("a refused Start keeps the modal open with Core's reason",
              m["open"] and "refused" in m["result"] and "run field 'serial' is required" in m["result"]
              and m["confirm"], m)
        script("var e=document.getElementById('prompt-field-0');e.value='SN-42';"
               "document.getElementById('prompt-field-1').value='Lab 2'")
        check("the fields are enabled again for a correction",
              not script("return document.getElementById('prompt-field-0').disabled"))
        script("document.getElementById('prompt-field-1').dispatchEvent(new KeyboardEvent('keydown',{key:'Enter'}))")
        wait_modal(lambda m: not m["open"])
        sent = stub("answers")["answers"][-1]
        check("the corrected Start is sent with every field, in order, as ui",
              sent.get("answered_by") == "ui" and sent.get("answer") == "confirmed"
              and sent.get("run_fields") == [{"name": "serial", "value": "SN-42"}, {"name": "room", "value": "Lab 2"}], sent)

        # 9. Done renders while sampling, with its text and Confirm only, and confirms
        stub("start", {"prompt_id": 2, "text": "Recording. Press Done to stop.", "deadline_in_ms": 120000,
                       "session": {"phase": "done", "run_fields": []}})
        m = wait_modal(lambda m: m["open"] and "Press Done" in m["text"])
        f = script("return {n:document.querySelectorAll('#prompt-fields input').length,"
                   "label:document.getElementById('prompt-confirm').textContent}")
        check("Done shows its text and a Done button, no fields and no Cancel",
              m["confirm"] and not m["cancel"] and f["n"] == 0 and f["label"] == "Done", (m, f))
        script("document.getElementById('prompt-confirm').click()")
        m = wait_modal(lambda m: not m["open"])
        sent = stub("answers")["answers"][-1]
        check("Done is answered confirmed with no run fields",
              not m["open"] and sent.get("answer") == "confirmed" and "run_fields" not in sent, sent)
        errs = script("return window.__errs || []")
        check("no uncaught JavaScript error", not errs, errs)
    finally:
        try: stub("reset")
        except Exception: pass
        try: rq("DELETE", base)
        except Exception: pass
    if fails:
        print(str(len(fails)) + " check(s) FAILED: " + ", ".join(fails)); sys.exit(1)
    print("every check passed")


main()
