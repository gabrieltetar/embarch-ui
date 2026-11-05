"""The Desk tab, in a real browser, against a scratch embarch-desk.

Starts its own embarch-desk on a scratch port with scratch config and data
directories (so the real desk, its tokens and its state are never touched),
seeds it through the desk's own HTTP surface — a standalone work item, a
session that a hook puts on it, a commit-less line, a flash — then starts an
embarch-ui pointed at that desk and a dead Core, and drives the tab: the card
renders and its tile is dealt into the hand above the work cards, with a chip on
the card that opens it, ✓ on a tile nobody has opened opens it instead of
sending, the dialog shows the lines, ✕ discards, the Dashboard is gone. The
chat's part (embarch-chat-desk-proposal.md): a session's row before the chat
ships and its one line after, Open chat through the mailbox (an ended chat
too), the card's notes from memory files written in a scratch Claude folder,
read again only when a note moves (a held line landing on assignment too),
the chat setting from the desk's view, saved on a click or Space without
touching what is typed in the panel or moving focus, Start work with and
without a prompt link, a proxy that answers only the UI's own page — a
bodyless POST from another origin included — and a page no other site can
frame. Desk notes (embarch-desk decision 74): the open desk's strip, a note
added, edited and deleted there and given to a session starting in the desk's
repo, an agent's badged, none of them a tile line, an item note or part of
the day, markup shown as text, and both limits: 500 refused in the box, the
desk's 4,000 refused in the desk's own words; an Add or Save made after the
desk was switched elsewhere writes nothing, an edit of a note deleted
elsewhere keeps its text, and Delete desk names the notes it deletes.

No connector is configured, so nothing reaches Linear or Harvest;
GitHub falls back to the machine's `gh` login for a read-only identity check.
Start work needs Linear, so its answer is stubbed in the page.

    cargo build && (cd ../embarch-desk && cargo build)
    geckodriver --port 4444 &
    python3 tests/browser/drive_desk.py
"""
import base64, json, os, shutil, subprocess, sys, tempfile, time, urllib.parse, urllib.request
import addr
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = os.environ.get("EMBARCH_UI_BIN", str(REPO / "target" / "debug" / "embarch-ui"))
DESK_BIN = os.environ.get("EMBARCH_DESK_BIN", str(REPO.parent / "embarch-desk" / "target" / "debug" / "embarch-desk"))
UI_PORT = addr.ui_port(4896)
DESK_PORT = os.environ.get("DESK_PORT", "4998")
UI = "http://127.0.0.1:" + UI_PORT
DESK = "http://127.0.0.1:" + DESK_PORT
BASE = addr.gecko_url()
SHOT = os.environ.get("DESK_SHOT")

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


def desk(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(DESK + path, data=data, method=method,
                                 headers={"Content-Type": "application/json", "Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read()
        return json.loads(raw.decode()) if raw else None   # a 204 has no body


def wait_for(url, secs=20):
    for _ in range(secs * 10):
        try:
            urllib.request.urlopen(url, timeout=1)
            return True
        except Exception:
            time.sleep(0.1)
    return False


def drive_desk_notes(js, until, settle, reload_view, real_click, type_into, replace_text, token, desk_id, repo, item, today):
    """Desk notes (embarch-desk decision 74): the open desk's standing rules,
    a strip under the head. Written, edited and deleted there; given to every
    session starting in the desk's repos; never a tile line, an item note or
    part of the day; their text never markup; a note over 500 characters
    refused in the box, a desk over 4,000 refused in the desk's own words."""
    notes_path = "/desks/%s/notes" % desk_id

    def stored():
        return desk("GET", notes_path, token=token)

    def strip_texts():
        return js("return Array.from(document.querySelectorAll('#desk-dnotes-list .desk-dnote-text')).map(function (e) { return e.textContent; })")

    def shows(text, secs=20):
        return until("return Array.from(document.querySelectorAll('#desk-dnotes-list .desk-dnote-text'))"
                     ".some(function (e) { return e.textContent === %s; })" % json.dumps(text), secs)

    def start_context(sid):
        return desk("POST", "/hook", {"hook_event_name": "SessionStart", "session_id": sid, "cwd": str(repo)}, token).get("context") or ""

    def count():
        return js("var c = document.getElementById('desk-dnotes-count'); return [c.textContent, c.classList.contains('over')]")

    strip = until("var s = document.getElementById('desk-dnotes'); return !s.hidden && s.textContent")
    check("the open desk draws its Desk notes strip under the head, with the desk's count of 4,000",
          bool(strip) and js("var n = document.querySelector('.desk-head').nextElementSibling; return n && n.id") == "desk-dnotes"
          and count() == ["0 of 4,000", False], [strip, count()])

    # Added in the strip: the count runs as it is typed, and the desk keeps it
    # as the user's. The desk's count is the list a session hears: each item
    # its "- ", its text and a line break, and an agent's its mark too.
    rule = "In repo X, the only build that matters is app Y on board Z."
    type_into("#desk-dnotes-new", rule)
    running = js("return [document.getElementById('desk-dnotes-count').textContent, document.getElementById('desk-dnotes-len').textContent]")
    check("the add box's count runs as it is typed, the desk's as a session hears it and the note's",
          running == ["%d of 4,000" % (len(rule) + 3), "%d of 500" % len(rule)], running)
    real_click("#desk-dnotes-add")
    shown = shows(rule)
    kept = stored()
    check("a note added in the strip shows there, kept by the desk as the user's, and the box empties",
          shown is True and [(n["text"], n["source"]) for n in kept["notes"]] == [(rule, "user")]
          and kept["used_chars"] == len(rule) + 3 and count()[0] == "%d of 4,000" % (len(rule) + 3)
          and js("return document.getElementById('desk-dnotes-new').value") == "", kept)
    note_id = kept["notes"][0]["id"] if kept["notes"] else "missing"

    ctx = start_context("dn1")
    check("a session starting in the desk's repo is given the note, under the desk's name",
          ('standing notes for the desk "Bench client"' in ctx) and ("- " + rule) in ctx, ctx[-400:])

    # An agent's note, written through the session, carries a badge; the user's none.
    agent_rule = "File F is owned by another engineer: report findings, never open a fix branch."
    by_agent = desk("POST", "/sessions/dn1/desk-note", {"text": agent_rule}, token)
    reload_view()
    shows(agent_rule)
    badges = js("return Array.from(document.querySelectorAll('#desk-dnotes-list .desk-dnote')).map(function (r) {"
                " return [r.querySelector('.desk-dnote-text').textContent, !!r.querySelector('.desk-dnote-agent')]; })")
    check("an agent's note shows with an agent badge, the user's without one",
          by_agent.get("desk_id") == desk_id and by_agent["note"]["source"] == "agent"
          and badges == [[rule, False], [agent_rule, True]], [by_agent, badges])
    agent_mark = " (added by an agent)"
    with_agent = len(rule) + 3 + len(agent_rule) + len(agent_mark) + 3
    check("and the desk's count takes the agent's mark too, as the strip shows it",
          stored()["used_chars"] == with_agent and count()[0] == "{:,} of 4,000".format(with_agent), count())

    # Never a tile line, an item note or part of the day: a tile with a line of
    # its own, a card's notes and the day are read with both notes on the desk.
    view = desk("GET", "/view", token=token)
    weekly = [c["id"] for c in view["cards"] if c["title"] == "Weekly call"][0]
    tile = desk("PATCH", "/tiles/%s@%s" % (weekly, today), {"hours": 0.5}, token)["id"]
    desk("POST", "/tiles/%s/line" % urllib.parse.quote(tile), {"text": "A line of the tile's own"}, token)
    reload_view()
    opened = until("var b = document.querySelector('#desk-hand [data-open-tile=%s]'); if (b && document.getElementById('desk-dialog').hidden) b.click();"
                   " var l = document.querySelector('#desk-dialog .desk-lines'); return !!l && l.textContent.indexOf(\"A line of the tile's own\") >= 0" % json.dumps(tile))
    dialog = js("var d = document.getElementById('desk-dialog'), n = document.getElementById('desk-note');"
                " return d.textContent + ' ' + (n ? n.value : '')")
    check("the notes are never a tile line: the tile's dialog holds its own line and neither note",
          opened is True and rule not in dialog and agent_rule not in dialog, dialog[:300])
    js("document.getElementById('desk-dialog-close').click(); return true")
    desk("POST", "/tiles/%s/discard" % urllib.parse.quote(tile), {}, token)
    js("document.querySelector('[data-notes=%s]').click(); return true" % json.dumps(item))
    card_notes = until("var b = document.getElementById('desk-notes-body'); return b && b.querySelector('.desk-notes-list') && b.textContent")
    check("nor an item note: the card's notes dialog reads its notes and neither desk note",
          bool(card_notes) and "Read once when a note moves" in card_notes and rule not in card_notes and agent_rule not in card_notes,
          (card_notes or "")[:300])
    js("document.getElementById('desk-notes-close').click(); return true")
    day = json.dumps(desk("GET", "/day/" + today, token=token))
    view = json.dumps({k: v for k, v in desk("GET", "/view", token=token).items() if k != "desk_notes"})
    shown_day = js("return document.getElementById('desk-day-body').textContent")
    check("nor part of the day: /day, the view outside desk_notes and Today's work hold neither",
          "Client sync call" in day and all(t not in x for t in (rule, agent_rule) for x in (day, view, shown_day)))

    # Edited in place. A view read while the editor is open keeps it and what
    # is typed in it; Save sends it as it stands.
    edited = "In repo X, only app Y on board Z is built; report another build's failures, never fix them."
    real_click('#desk-dnotes-list [data-dnote-edit="%s"]' % note_id)
    box = until("var b = document.querySelector('#desk-dnotes-list li.editing textarea'); return !!b && document.activeElement === b && b.value")
    check("Edit opens the note's text in place, focused", box == rule, box)
    replace_text("#desk-dnotes-list li.editing textarea", edited)
    js("window.__dnoteBox = document.querySelector('#desk-dnotes-list li.editing textarea'); document.activeElement.blur(); return true")
    reload_view()
    held = until("var b = document.querySelector('#desk-dnotes-list li.editing textarea'); return !!b && b !== window.__dnoteBox && b.value")
    check("a view read while the editor is open draws it again with what was typed", held == edited, held)
    check("and the count runs with the edit in place of the note",
          count() == ["{:,} of 4,000".format(with_agent - len(rule) + len(edited)), False], count())
    real_click('#desk-dnotes-list [data-dnote-save="%s"]' % note_id)
    shown = shows(edited)
    after = stored()["notes"]
    check("Save puts the edit on the strip and in the desk, the same note still the user's",
          shown is True and [(n["id"], n["text"], n["source"]) for n in after][:1] == [(note_id, edited, "user")]
          and not js("return !!document.querySelector('#desk-dnotes-list li.editing')"), after)
    ctx = start_context("dn2")
    check("the next session to start is given the edit, not the old text", edited in ctx and rule not in ctx, ctx[-400:])

    # What a note says renders as text, in the strip and in its editor.
    markup = "<b>x</b> stays text <img src=q onerror=window.__dn=1>"
    type_into("#desk-dnotes-new", markup)
    real_click("#desk-dnotes-add")
    shows(markup)
    got = js("return {el: document.querySelectorAll('#desk-dnotes b, #desk-dnotes img').length, p: window.__dn === undefined ? 0 : 1}")
    check("a note holding <b>x</b> renders as text, not HTML", got == {"el": 0, "p": 0} and markup in strip_texts(), [got, strip_texts()])
    markup_id = ([n["id"] for n in stored()["notes"] if n["text"] == markup] or ["missing"])[0]
    real_click('#desk-dnotes-list [data-dnote-edit="%s"]' % markup_id)
    in_box = until("var b = document.querySelector('#desk-dnotes-list li.editing textarea'); return !!b && b.value")
    check("and its editor holds the same text", in_box == markup
          and js("return document.querySelectorAll('#desk-dnotes b, #desk-dnotes img').length") == 0, in_box)
    real_click("#desk-dnotes-list [data-dnote-cancel]")
    check("Cancel puts the note back as it was", shows(markup, 5) is True and not js("return !!document.querySelector('#desk-dnotes-list li.editing')"))

    # 501 characters are refused in the box, and nothing is cut: Add is off,
    # the box says why, and even a click that reaches the handler sends nothing.
    before = stored()["used_chars"]
    type_into("#desk-dnotes-new", "a" * 501)
    box_state = ("return {off: document.getElementById('desk-dnotes-add').disabled, len: document.getElementById('desk-dnotes-len').textContent,"
                 " over: document.getElementById('desk-dnotes-len').classList.contains('over'),"
                 " why: document.getElementById('desk-dnotes-status').textContent, kept: document.getElementById('desk-dnotes-new').value.length}")
    refused = js(box_state)
    js("var b = document.getElementById('desk-dnotes-add'); b.disabled = false; b.click(); return true")
    settle()
    check("the add box refuses 501 characters: Add is off, the box says why and keeps all 501, and nothing reaches the desk",
          refused["off"] and refused["len"] == "501 of 500" and refused["over"] and "at most 500" in refused["why"]
          and refused["kept"] == 501 and stored()["used_chars"] == before, refused)
    type_into("#desk-dnotes-new", "\ue003")   # Backspace: 500 is a note
    at_limit = js(box_state)
    check("one character fewer is a note again", not at_limit["off"] and at_limit["len"] == "500 of 500" and not at_limit["over"]
          and at_limit["why"] == "", at_limit)
    js("var b = document.getElementById('desk-dnotes-new'); b.value = ''; b.dispatchEvent(new Event('input', {bubbles: true})); return true")

    def set_new(text):
        js("var b = document.getElementById('desk-dnotes-new'); b.value = arguments[0]; b.dispatchEvent(new Event('input', {bubbles: true})); return true", text)

    # A line break counts 3 against the 500, for the indent a session reads;
    # and a note counts as the desk keeps it: blank lines and trailing blanks
    # dropped. The box's count before Add is the desk's after it.
    set_new("x" * 249 + "\n" + "x" * 249)
    two = js(box_state)
    check("a line break counts 3: 499 characters on two lines are refused in the box as 501",
          two["off"] and two["len"] == "501 of 500" and "a line break counting 3" in two["why"], two)
    used0 = stored()["used_chars"]
    set_new("Line one, rule X.  \n\n   \nLine two, rule Y.\t\n")
    typed = [count()[0], js("return document.getElementById('desk-dnotes-len').textContent")]
    real_click("#desk-dnotes-add")
    kept_as = "Line one, rule X.\nLine two, rule Y."
    shows(kept_as)
    after = stored()
    check("a note with blank lines and trailing blanks counts as the desk keeps it, before Add as after",
          typed == ["{:,} of 4,000".format(used0 + len(kept_as) + 2 + 3), "%d of 500" % (len(kept_as) + 2)]
          and after["used_chars"] == used0 + len(kept_as) + 2 + 3 and kept_as in [n["text"] for n in after["notes"]], [typed, after["used_chars"]])
    for n in after["notes"]:
        if n["text"] == kept_as:
            desk("DELETE", "%s/%s" % (notes_path, n["id"]), token=token)

    # The desk's 4,000: filled through its own surface to 100 under, then a
    # note one character too long for what is left typed here. The desk
    # refuses it, and the box says so in the desk's words and keeps the text.
    fillers = []
    used = stored()["used_chars"]
    while used + 503 <= 3900:
        fillers.append(desk("POST", notes_path, {"text": "f" * 500}, token)["note"]["id"])
        used = stored()["used_chars"]
    if 3900 - used - 3 > 0:
        fillers.append(desk("POST", notes_path, {"text": "f" * (3900 - used - 3)}, token)["note"]["id"])
        used = stored()["used_chars"]
    over = "o" * (4000 - used - 3 + 1)
    reload_view()
    until("return document.getElementById('desk-dnotes-count').textContent === %s" % json.dumps("{:,} of 4,000".format(used)))
    set_new(over)
    running = count()
    real_click("#desk-dnotes-add")
    said = until("return document.getElementById('desk-dnotes-status').textContent")
    try:
        desk("POST", notes_path, {"text": over}, token)
        direct = None
    except urllib.error.HTTPError as e:
        direct = (e.code, json.loads(e.read().decode()).get("error"))
    check("a note past the desk's 4,000 shows the count over, then the desk's own refusal under the box, the text kept",
          running == ["4,001 of 4,000", True] and direct is not None and direct[0] == 400 and said == direct[1]
          and js("return document.getElementById('desk-dnotes-new').value") == over and stored()["used_chars"] == used,
          [running, said, direct])
    js("var b = document.getElementById('desk-dnotes-new'); b.value = ''; b.dispatchEvent(new Event('input', {bubbles: true})); return true")
    for f in fillers:
        desk("DELETE", "%s/%s" % (notes_path, f), token=token)

    # Deleted here, after a confirm; a no keeps it.
    js("window.__asked = []; window.confirm = function (m) { window.__asked.push(m); return false; }; return true")
    real_click('#desk-dnotes-list [data-dnote-delete="%s"]' % note_id)
    settle()
    asked = js("return window.__asked")
    check("Delete asks first, quoting the note, and a no keeps it",
          len(asked) == 1 and edited[:40] in asked[0] and note_id in [n["id"] for n in stored()["notes"]], asked)
    js("window.confirm = function (m) { window.__asked.push(m); return true; }; return true")
    real_click('#desk-dnotes-list [data-dnote-delete="%s"]' % note_id)
    gone = until("return !document.querySelector('#desk-dnotes-list [data-dnote=%s]')" % json.dumps(note_id))
    check("a yes takes it off the strip and out of the desk",
          gone is True and note_id not in [n["id"] for n in stored()["notes"]] and edited not in strip_texts(), strip_texts())
    ctx = start_context("dn3")
    check("and the next session to start is not given it", edited not in ctx and agent_rule in ctx, ctx[-400:])

    # The open desk switched elsewhere while a note is typed (review M2). With
    # a box in the strip focused a pushed view is held back, so the strip still
    # shows the desk it was drawn for; a write then draws the tab again, writes
    # nothing and says so. The page's own view comes with the server's push
    # every 10 s, so each switch waits that long with the focus kept in the box.
    beta = desk("POST", "/desks", {"name": "Desk Beta"}, token)["id"]
    beta_path = "/desks/%s/notes" % beta

    def switched_while_typing(to):
        desk("POST", "/open", {"desk": to}, token)
        time.sleep(13)
        return js("return document.getElementById('desk-head-name').textContent")

    def head_is(name):
        return "document.getElementById('desk-head-name').textContent === %s" % json.dumps(name)

    before = [n["id"] for n in stored()["notes"]]
    raced = "Typed while the open desk was Bench client."
    type_into("#desk-dnotes-new", raced)
    stale = switched_while_typing(beta)
    real_click("#desk-dnotes-add")
    said = until("return %s && document.getElementById('desk-dnotes-status').textContent" % head_is("Desk Beta"))
    check("a note typed while the desk is switched elsewhere is not written: Add draws the tab again, keeps the text and says why",
          stale == "Bench client" and bool(said) and "from Bench client to Desk Beta" in said and "nothing was added" in said
          and js("return document.getElementById('desk-dnotes-new').value") == raced
          and [n["id"] for n in stored()["notes"]] == before and desk("GET", beta_path, token=token)["notes"] == [], [stale, said])
    real_click("#desk-dnotes-add")
    check("and Add once more, the new desk on screen, puts it on that desk",
          shows(raced) is True and [n["text"] for n in desk("GET", beta_path, token=token)["notes"]] == [raced]
          and [n["id"] for n in stored()["notes"]] == before)

    agent_id = by_agent["note"]["id"]
    desk("POST", "/open", {"desk": desk_id}, token)
    reload_view()
    until("return %s && !document.getElementById('desk-dnotes').hidden" % head_is("Bench client"))
    js("var b = document.getElementById('desk-bench-later'); if (b) b.click(); return true")
    real_click('#desk-dnotes-list [data-dnote-edit="%s"]' % agent_id)
    until("var b = document.querySelector('#desk-dnotes-list li.editing textarea'); return !!b && document.activeElement === b")
    reworded = "File F belongs to another engineer: report findings to them, never open a fix branch."
    replace_text("#desk-dnotes-list li.editing textarea", reworded)
    stale = switched_while_typing(beta)
    real_click('#desk-dnotes-list [data-dnote-save="%s"]' % agent_id)
    said = until("return %s && document.getElementById('desk-error').textContent" % head_is("Desk Beta"))
    check("an edit saved after the desk was switched elsewhere is not written: the tab is drawn again and the flash says so",
          stale == "Bench client" and bool(said) and "before Save, so nothing was written" in said
          and [n["text"] for n in stored()["notes"] if n["id"] == agent_id] == [agent_rule]
          and not js("return !!document.querySelector('#desk-dnotes-list li.editing')"), [stale, said])
    desk("POST", "/open", {"desk": desk_id}, token)
    reload_view()
    back = until("var b = document.querySelector('#desk-dnotes-list li.editing textarea'); return %s && !!b && b.value" % head_is("Bench client"))
    js("var b = document.getElementById('desk-bench-later'); if (b) b.click(); return true")
    check("the edit is back with its text once its desk is open again", back == reworded, back)
    real_click('#desk-dnotes-list [data-dnote-save="%s"]' % agent_id)
    check("and Save then writes it to that note",
          shows(reworded) is True and [n["text"] for n in stored()["notes"] if n["id"] == agent_id] == [reworded])

    # A note deleted elsewhere while its editor is open: Save keeps the typed
    # text and says what happened, and Add again keeps it as a new note.
    real_click('#desk-dnotes-list [data-dnote-edit="%s"]' % agent_id)
    until("return !!document.querySelector('#desk-dnotes-list li.editing textarea')")
    kept_text = "Kept although the note went: report findings, never open a fix branch."
    replace_text("#desk-dnotes-list li.editing textarea", kept_text)
    desk("DELETE", "%s/%s" % (notes_path, agent_id), token=token)
    real_click('#desk-dnotes-list [data-dnote-save="%s"]' % agent_id)
    orphan = until("var r = document.querySelector('#desk-dnotes-list li.editing.gone');"
                   " return !!r && !document.querySelector('#desk-dnotes-list [data-dnote-edit=%s]') && {text: r.querySelector('textarea').value,"
                   " why: r.querySelector('.desk-dnotes-err').textContent, again: !!r.querySelector('[data-dnote-readd]')}" % json.dumps(agent_id))
    check("Save on a note deleted elsewhere keeps the typed text, says the note went, and offers Add again",
          bool(orphan) and orphan["text"] == kept_text and "deleted elsewhere" in orphan["why"] and orphan["again"], orphan)
    real_click("#desk-dnotes-list [data-dnote-readd]")
    check("Add again keeps it as a new note on the desk",
          shows(kept_text) is True and not js("return !!document.querySelector('#desk-dnotes-list li.editing')")
          and [n["source"] for n in stored()["notes"] if n["text"] == kept_text] == ["user"])

    # Deleting a desk deletes its notes for good, and the question says so (review, low 2).
    js("window.__asked = []; window.confirm = function (m) { window.__asked.push(m); return false; }; return true")
    js("document.dispatchEvent(new CustomEvent('embarch:desk-form', {detail: {id: arguments[0]}})); return true", beta)
    until("return !!document.querySelector('[data-form-delete]')", 40)
    real_click("[data-form-delete]")
    asked = until("return window.__asked.length && window.__asked")
    check("Delete desk names the notes it deletes with the desk",
          bool(asked) and "Its 1 desk note is deleted with it, for good." in asked[0] and beta in [d["id"] for d in desk("GET", "/desks", token=token)["desks"]], asked)
    js("window.confirm = function (m) { window.__asked.push(m); return true; }; return true")
    real_click("[data-form-delete]")
    until("return document.getElementById('desk-form').hidden")
    try:
        desk("GET", beta_path, token=token)
        beta_notes = "still there"
    except urllib.error.HTTPError as e:
        beta_notes = e.code
    check("and a yes deletes the desk and its notes", beta not in [d["id"] for d in desk("GET", "/desks", token=token)["desks"]] and beta_notes == 404,
          beta_notes)

    for n in stored()["notes"]:
        desk("DELETE", "%s/%s" % (notes_path, n["id"]), token=token)
    for sid in ("dn1", "dn2", "dn3"):
        desk("POST", "/hook", {"hook_event_name": "SessionEnd", "session_id": sid, "cwd": str(repo)}, token)
    reload_view()
    until("return document.getElementById('desk-dnotes-count').textContent === '0 of 4,000'")


def main():
    scratch = Path(tempfile.mkdtemp(prefix="drive-desk-"))
    # A scratch Claude folder too: the desk reads memory files from it, and
    # never imports the machine's own.
    env = {**os.environ, "EMBARCH_DESK_CONFIG_DIR": str(scratch / "cfg"), "EMBARCH_DESK_DATA_DIR": str(scratch / "data"),
           "EMBARCH_DESK_PORT": DESK_PORT, "CLAUDE_CONFIG_DIR": str(scratch / "claude")}
    cfg = scratch / "ui.toml"
    cfg.write_text('[core]\nbase_url = "http://127.0.0.1:1"\ntoken = "x"\n')
    procs = []
    try:
        procs.append(subprocess.Popen([DESK_BIN, "serve"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        check("the scratch desk starts", wait_for(DESK + "/health"))
        token = (scratch / "cfg" / "desk-token").read_text().strip()

        def hook(ev):
            subprocess.run([DESK_BIN, "hook"], input=json.dumps(ev), env=env, capture_output=True, text=True)

        def mailbox_for(folder):
            return desk("GET", "/mailbox?folder=" + urllib.parse.quote(folder), token=token)["entries"]

        repo = scratch / "repo"
        repo.mkdir()
        item = desk("POST", "/items", {"title": "Client sync call"}, token)["id"]
        start = {"hook_event_name": "SessionStart", "session_id": "s1", "cwd": str(repo)}
        out = subprocess.run([DESK_BIN, "hook"], input=json.dumps(start), env=env, capture_output=True, text=True).stdout
        check("a new session is asked which work item it is", "desk_set_work_item" in out, out[:120])
        desk("POST", "/sessions/s1/assign", {"item_id": item}, token)
        desk("POST", "/sessions/s1/line", {"text": "Agreed the bootload test plan"}, token)
        hook({"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo),
              "tool_name": "mcp__embarch-api__flash", "tool_input": {}, "tool_response": {}})
        today = desk("GET", "/view", token=token)["today"]
        desk("PATCH", "/tiles/%s@%s" % (item, today), {"hours": 1.5}, token)

        procs.append(subprocess.Popen([BIN], env={**env, "EMBARCH_UI_CONFIG": str(cfg), "EMBARCH_UI_PORT": UI_PORT},
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        check("the scratch UI starts", wait_for(UI + "/"))

        sid = wd("POST", BASE + "/session", {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"]}}}})["sessionId"]
        S = BASE + "/session/" + sid
        try:
            def js(src, *args):
                return wd("POST", S + "/execute/sync", {"script": src, "args": list(args)})

            def settle():
                time.sleep(0.6)

            def until(src, secs=20):
                v = None
                for _ in range(secs * 5):
                    v = js(src)
                    if v:
                        return v
                    time.sleep(0.2)
                return v

            def reload_view():
                # The nav item reads /view at once, rather than waiting for the next push.
                js("document.querySelector('.nav-item[data-tab=desk]').click(); return true")

            def real_click(css):
                el = wd("POST", S + "/element", {"using": "css selector", "value": css})
                wd("POST", S + "/element/" + list(el.values())[0] + "/click", {})

            def type_into(css, text):
                el = wd("POST", S + "/element", {"using": "css selector", "value": css})
                wd("POST", S + "/element/" + list(el.values())[0] + "/value", {"text": text})

            def press(key):
                wd("POST", S + "/actions", {"actions": [{"type": "key", "id": "kbd", "actions": [
                    {"type": "keyDown", "value": key}, {"type": "keyUp", "value": key}]}]})
                wd("DELETE", S + "/actions")

            def setting_becomes(value, secs=6):
                for _ in range(secs * 5):
                    if desk("GET", "/settings", token=token).get("chat_panel") is value:
                        return True
                    time.sleep(0.2)
                return False

            def stub_fetch(pattern, status, error):
                # Answer one route with an error, once per call, until unstub_fetch().
                js("var re = new RegExp(arguments[0]), st = arguments[1], msg = arguments[2];"
                   "window.__realFetch = window.__realFetch || window.fetch;"
                   "window.fetch = function (u) { if (re.test(u)) return Promise.resolve(new Response(JSON.stringify({error: msg}), {status: st}));"
                   " return window.__realFetch.apply(window, arguments); }; return true", pattern, status, error)

            def unstub_fetch():
                js("if (window.__realFetch) window.fetch = window.__realFetch; return true")

            def replace_text(css, text):
                # Ctrl+A, then the text over the selection: the keyboard's way.
                # Two calls, because geckodriver holds a modifier to the end of
                # one, and a NULL key inside it does not let go.
                type_into(css, "\ue009a")
                type_into(css, text)

            wd("POST", S + "/window/rect", {"width": 1400, "height": 950})
            wd("POST", S + "/url", {"url": UI + "/#desk"})
            for _ in range(50):
                if js("return document.querySelectorAll('.desk-card').length") > 0:
                    break
                time.sleep(0.2)
            check("the nav says Desk and nothing says Dashboard",
                  js("return document.querySelector('.nav-item[data-tab=desk] .nav-label').textContent") == "Desk"
                  and js("return !document.querySelector('[data-tab=dashboard]') && !document.getElementById('dashboard-alerts-list')"))
            check("the Desk tab is the one showing", js("return document.querySelector('.tab-panel[data-tab=desk]').classList.contains('active')"))
            check("an old #dashboard link lands on the Desk",
                  js("location.hash = '#dashboard'; return true") and (settle() or True)
                  and js("return document.querySelector('.tab-panel[data-tab=desk]').classList.contains('active')"))
            check("the card renders with its title", js("return document.querySelector('.desk-card-title').textContent") == "Client sync call")
            check("its bench activity shows", "flash ×1" in js("return document.querySelector('.desk-card-meta').textContent"))
            tile = js("var t = document.querySelector('#desk-hand .desk-tile'); return t && {hours: t.querySelector('.desk-tile-hours').textContent, title: t.querySelector('.desk-tile-title').textContent, ok: !!t.querySelector('.desk-ok'), no: !!t.querySelector('.desk-no')}")
            check("today's tile is a card in the hand with its item, hours and ✓ ✕",
                  tile and tile["hours"] == "1h30" and tile["title"] == "Client sync call" and tile["ok"] and tile["no"], tile)
            check("the hand sits above the pick-up list and the cards",
                  js("var h = document.getElementById('desk-hand'), p = document.getElementById('desk-pickup'); return !!(h.compareDocumentPosition(p) & Node.DOCUMENT_POSITION_FOLLOWING)"))
            check("the work card holds no tile, only a chip that opens it",
                  js("return !document.querySelector('.desk-card .desk-tile') && /1h30/.test(document.querySelector('.desk-card .desk-card-time .desk-mini').textContent)"))
            # Until the chat ships (chat decision 4) the setting is off, and a
            # session keeps its row from before: Answer, Jump and its last reply.
            for ev in ({"hook_event_name": "Stop", "session_id": "s1", "cwd": str(repo),
                        "last_assistant_message": "Summary: the loader needs 432 KB."},
                       {"hook_event_name": "Notification", "session_id": "s1", "cwd": str(repo),
                        "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}):
                hook(ev)
            row_js = ("var r = document.querySelector('.desk-session'); var a = r && r.querySelector('a');"
                      " return r && {text: r.textContent, answer: a ? a.getAttribute('href') : null,"
                      " reply: (r.querySelector('.desk-session-reply') || {}).textContent || '',"
                      " jump: !!r.querySelector('[data-jump-session]'), chat: !!r.querySelector('[data-open-chat]'),"
                      " line: !!r.querySelector('.desk-session-state'), links: r.querySelectorAll('a').length}")
            row = until(row_js.replace("return r &&", "return r && r.textContent.indexOf('432 KB') >= 0 &&"))
            check("with the chat setting off, a session keeps Answer, Jump and its last reply (chat decision 4)",
                  bool(row) and (row["answer"] or "").endswith("/code/s1") and row["jump"] and not row["chat"]
                  and "432 KB" in row["reply"] and not row["line"], row)

            # The chat setting is the desk's, rendered from each view it pushes.
            js("document.getElementById('desk-settings-toggle').click(); return true")
            toggle = until("var c = document.getElementById('desk-chat-panel'); return c && !c.disabled && {on: c.checked}")
            check("the chat setting shows in the panel, off by default", toggle == {"on": False}, toggle)
            desk("PUT", "/settings", {"chat_panel": True}, token)
            reload_view()
            check("the setting changed elsewhere reaches the open panel with the next view",
                  until("return document.getElementById('desk-chat-panel').checked") is True)
            desk("PUT", "/settings", {"chat_panel": False}, token)
            reload_view()
            until("return !document.getElementById('desk-chat-panel').checked")
            # What is typed in the panel and not saved yet survives a click on
            # the setting, and the box keeps focus (desk review r2, finding 2);
            # a box with focus must not hold every pushed view back (r1, 2).
            typed = ["Bench workspace", "lin-not-a-token", "/w/never-tracked"]
            for css, text in zip(("#desk-conn-label", "#desk-conn-token", "#desk-ignore"), typed):
                type_into(css, text)
            typed_js = ("return [document.getElementById('desk-conn-label').value, document.getElementById('desk-conn-token').value,"
                        " document.getElementById('desk-ignore').value.split('\\n').pop()]")
            real_click("#desk-chat-panel")
            check("turning the setting on with a click reaches the desk", setting_becomes(True))
            settle()
            check("and leaves what was typed in the panel as it was", js(typed_js) == typed, js(typed_js))
            focus_js = "return document.activeElement && document.activeElement.id"
            check("and keeps focus on the box", js(focus_js) == "desk-chat-panel", js(focus_js))
            pushed_id = desk("POST", "/items", {"title": "Pushed after the toggle"}, token)["id"]
            check("a card made elsewhere after the click still arrives by SSE",
                  until("return document.getElementById('desk-cards').textContent.indexOf('Pushed after the toggle') >= 0", 25) is True)
            check("and the pushed view leaves the typed text and the focus where they were",
                  js(typed_js) == typed and js(focus_js) == "desk-chat-panel", [js(typed_js), js(focus_js)])
            desk("POST", "/items/%s/archive" % pushed_id, {"value": True}, token)   # out of the way of the tile checks
            # The keyboard: Space toggles the focused box, and focus stays on it.
            if js(focus_js) != "desk-chat-panel":
                js("document.getElementById('desk-chat-panel').focus(); return true")
            press(" ")
            check("Space on the focused box turns the setting off at the desk", setting_becomes(False))
            settle()
            check("and leaves focus on the box", js(focus_js) == "desk-chat-panel", js(focus_js))
            if js(focus_js) != "desk-chat-panel":
                js("document.getElementById('desk-chat-panel').focus(); return true")
            press(" ")
            check("Space again turns it back on", setting_becomes(True))

            # With it on, a session is one line — state, what it waits for, age
            # — with Jump; Answer and the last reply have left it (chat decision 32).
            row = until(row_js.replace("return r &&", "return r && !!r.querySelector('.desk-session-state') &&"))
            check("with it on, a terminal session is one line: its state, what it waits for, its age, and Jump",
                  bool(row) and "waiting" in row["text"] and "for permission prompt" in row["text"]
                  and ("ago" in row["text"] or "just now" in row["text"]) and row["jump"] and not row["chat"], row)
            check("Answer and the last reply have left the row", bool(row) and row["links"] == 0 and "432 KB" not in row["text"], row)
            js("document.getElementById('desk-settings-toggle').click(); return true")

            # A chat the panel runs gets Open chat: a focus request left in
            # the desk's mailbox for its folder, then that folder's link.
            chat_dir = scratch / "repo-ABC-1"
            chat_dir.mkdir()
            desk("POST", "/sessions/c1/assign", {"item_id": item, "client": "embarch-chat"}, token)
            hook({"hook_event_name": "SessionStart", "session_id": "c1", "cwd": str(chat_dir)})
            rows = None
            for _ in range(100):
                rows = js("return Array.from(document.querySelectorAll('.desk-session')).map(function (r) {"
                          " return {chat: !!r.querySelector('[data-open-chat]'), jump: !!r.querySelector('[data-jump-session]')}; })")
                if any(r["chat"] for r in rows):
                    break
                time.sleep(0.2)
            check("a chat the panel runs gets Open chat, and only the other session Jump",
                  sorted((r["chat"], r["jump"]) for r in rows) == [(False, True), (True, False)], rows)

            def mailbox():
                return desk("GET", "/mailbox?folder=" + urllib.parse.quote(str(chat_dir)), token=token)["entries"]

            # The button carries the desk's link for the chat's folder. A
            # vscode:// link leaves no trace a headless browser can see, so the
            # same button is pointed at a page on this origin, and the tab
            # must follow it once the focus request is in (desk review r2, 4d).
            chat_uri = [r.get("folder_uri") for c in desk("GET", "/view", token=token)["cards"]
                        for r in c.get("live_sessions", []) if r.get("session_id") == "c1"]
            shown_uri = js("return document.querySelector('[data-open-chat]').getAttribute('data-chat-uri')")
            check("Open chat carries the desk's link for the chat's folder",
                  bool(chat_uri) and chat_uri[0] and chat_uri[0].startswith("vscode://") and shown_uri == chat_uri[0], [shown_uri, chat_uri])
            follow = UI + "/?chat-link-followed#desk"
            js("var b = document.querySelector('[data-open-chat]'); b.setAttribute('data-chat-uri', arguments[0]); b.click(); return true", follow)
            mail = []
            for _ in range(30):
                mail = mailbox()
                if mail:
                    break
                time.sleep(0.2)
            check("Open chat leaves a focus request for the chat's folder in the desk's mailbox",
                  [(m["kind"], m["session_id"]) for m in mail] == [("focus", "c1")], mail)
            followed = until("return location.href === %s && document.readyState === 'complete' && location.href" % json.dumps(follow))
            check("and then follows the chat folder's link", followed == follow, followed or wd("GET", S + "/url"))
            for m in mail:
                desk("POST", "/mailbox/%s/take" % m["id"], {}, token)
            wd("POST", S + "/url", {"url": UI + "/#desk"})
            until("return location.search === '' && !!document.querySelector('[data-open-chat]')")

            # A mailbox that refuses: the error shows and no link is followed.
            stub_fetch("/api/desk/mailbox$", 500, "the mailbox refused it")
            href = js("return location.href")
            js("document.querySelector('[data-open-chat]').click(); return true")
            check("Open chat that the desk refuses says why and opens nothing",
                  until("return document.getElementById('desk-error').textContent.indexOf('the mailbox refused it') >= 0", 5) is True
                  and js("return location.href") == href)
            unstub_fetch()

            # A chat whose window closed keeps its line, and Open chat resumes it (32).
            hook({"hook_event_name": "SessionEnd", "session_id": "c1", "cwd": str(chat_dir)})
            reload_view()
            ended = until("var b = document.querySelector('.desk-session.ended [data-open-chat=c1]'); return b && b.title")
            check("an ended chat keeps its line on the card, with Open chat to resume it", bool(ended) and "resume" in ended, ended)
            mail = []
            if js("var b = document.querySelector('.desk-session.ended [data-open-chat=c1]'); if (b) b.click(); return !!b"):
                for _ in range(30):
                    mail = mailbox()
                    if mail:
                        break
                    time.sleep(0.2)
            check("Open chat on an ended chat leaves the focus request its window resumes it from",
                  [(m["kind"], m["session_id"]) for m in mail] == [("focus", "c1")], mail)
            for m in mail:
                desk("POST", "/mailbox/%s/take" % m["id"], {}, token)
            # With the setting off the rows are as before the chat: no ended chat.
            desk("PUT", "/settings", {"chat_panel": False}, token)
            reload_view()
            off = until("var r = document.querySelectorAll('.desk-session'); return r.length === 1 && r[0].querySelector('a') && r.length")
            check("with the setting off again, the ended chat stays off the card and Answer is back", off == 1,
                  js("return Array.from(document.querySelectorAll('.desk-session')).map(function (r) { return r.textContent; })"))
            desk("PUT", "/settings", {"chat_panel": True}, token)
            reload_view()

            # Notes: each at its newest state on the card, every one behind a
            # click (chat decision 46). A memory file written twice keeps both
            # versions; the card shows the second.
            mem = scratch / "claude" / "projects" / "-w-fw" / "memory"
            mem.mkdir(parents=True)

            def write_mem(name, text, tool="Write"):
                path = mem / name
                path.write_text(text)
                hook({"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo), "tool_name": tool,
                      "tool_input": {"file_path": str(path)}, "tool_response": {}})

            write_mem("abc-1-state.md", '---\nname: abc-1-state\ndescription: "ABC-1: branch made"\n---\n\nBranch abc-1-loader.\n')
            write_mem("abc-1-state.md", '---\nname: abc-1-state\ndescription: "ABC-1: PR open, flash pending"\n---\n\nBranch abc-1-loader, PR open.\n', "Edit")
            desk("POST", "/sessions/s1/line", {"text": "The loader needs 432 KB of RAM", "kind": "finding"}, token)
            card = None
            for _ in range(100):
                card = js("var n = document.querySelector('.desk-notes'); return n && n.textContent")
                if card and "432 KB of RAM" in card:
                    break
                time.sleep(0.2)
            check("a card shows each note's newest state: the memory file's latest text, the latest lines with their kind",
                  bool(card) and "abc-1-state" in card and "PR open, flash pending" in card and "branch made" not in card
                  and "finding" in card and "Agreed the bootload test plan" in card, card)
            check("the card's notes have a short accessible name",
                  (js("return document.querySelector('.desk-notes').getAttribute('aria-label')") or "").startswith("Notes, "))
            js("document.querySelector('.desk-notes').click(); return true")
            full = ""
            for _ in range(30):
                full = js("return document.getElementById('desk-notes').textContent")
                if "branch made" in full:
                    break
                time.sleep(0.2)
            check("a click opens every note, newest first, a memory file's earlier version too",
                  js("return !document.getElementById('desk-notes').hidden")
                  and full.find("432 KB of RAM") < full.find("PR open, flash pending") < full.find("branch made")
                  and "Agreed the bootload test plan" in full, full[:300])
            a11y = js("var d = document.getElementById('desk-notes'), t = document.getElementById(d.getAttribute('aria-labelledby'));"
                      " return {name: t ? t.textContent : null, focus: document.activeElement.id}")
            check("the notes dialog is named by its title and takes focus on Close",
                  (a11y["name"] or "").startswith("Notes — ") and a11y["focus"] == "desk-notes-close", a11y)
            if SHOT:
                Path(SHOT.replace(".png", "-notes.png")).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))
            js("document.getElementById('desk-notes-close').click(); return true")
            check("Close puts the list away", js("return document.getElementById('desk-notes').hidden"))
            check("and gives focus back to the card's notes", js("return document.activeElement.getAttribute('data-notes')") == item)

            # What a memory file or a line says renders as text, never as markup;
            # a YAML-quoted or folded description reads as its writer meant it.
            write_mem("x<img src=q onerror=window.__p0=1>.md",
                      '---\nname: x\ndescription: "<img src=x onerror=window.__p1=1>"\n---\n\n<script>window.__p2=1</script><b>bold</b>\n')
            desk("POST", "/sessions/s1/line", {"text": "<img src=y onerror=window.__p3=1> a line", "kind": "decision"}, token)
            write_mem("quoted.md", '---\nname: quoted\ndescription: "folded into \\"budget the ports\\" and pushed"\n---\n\nbody\n')
            write_mem("folded.md", "---\nname: folded\ndescription: >-\n  a description folded\n  across two lines\ntype: project\n---\n\nbody\n")
            card = until("var n = document.querySelector('.desk-notes'); return n && n.textContent.indexOf('across two lines') >= 0 && n.textContent") \
                or js("var n = document.querySelector('.desk-notes'); return n ? n.textContent : ''")
            check("a quoted description shows its quotes, not their escapes",
                  bool(card) and 'folded into "budget the ports" and pushed' in card and '\\"' not in card, card)
            check("a folded description shows its text, not its marker", bool(card) and "a description folded across two lines" in card, card)
            injected = "return {el: document.querySelectorAll(arguments[0] + ' img, ' + arguments[0] + ' script, ' + arguments[0] + ' .desk-note b, ' + arguments[0] + ' .desk-notes-text b').length," \
                       " p: [window.__p0, window.__p1, window.__p2, window.__p3].filter(function (x) { return x !== undefined; }).length}"
            got = js(injected, "#desk-cards")
            check("a memory file's name, description and a line render as text on the card", got == {"el": 0, "p": 0} and "<img src=x" in card, got)
            js("document.querySelector('.desk-notes').click(); return true")
            until("return document.getElementById('desk-notes').textContent.indexOf('window.__p2') >= 0")
            got = js(injected, "#desk-notes")
            check("and in the full list", got == {"el": 0, "p": 0}, got)
            js("document.getElementById('desk-notes-close').click(); return true")

            # Many memory files: the card shows the three newest, and counts the rest.
            write_mem("topic-1.md", '---\nname: t1\ndescription: "topic one"\n---\n\nbody\n')
            write_mem("topic-2.md", '---\nname: t2\ndescription: "topic two"\n---\n\nbody\n')
            files = until("var n = document.querySelector('.desk-notes'); return n && n.textContent.indexOf('topic two') >= 0 &&"
                          " {rows: n.querySelectorAll('.desk-note-file').length, label: n.querySelector('.desk-label').textContent}")
            check("a card shows its three newest memory files and counts the rest in its label",
                  bool(files) and files["rows"] == 3 and "3 more files" in files["label"], files)

            # A card reads its notes again only when a note moves notes_ms, not
            # on every hook; a read that fails is made again on the next view.
            js("window.__reads = 0; window.__failReads = 0; var f = window.fetch;"
               " window.fetch = function (u) { if (/\\/notes$/.test(u)) { window.__reads++;"
               "   if (window.__failReads > 0) { window.__failReads--; return Promise.resolve(new Response('{}', {status: 502})); } }"
               " return f.apply(window, arguments); }; return true")
            for _ in range(3):
                hook({"hook_event_name": "PostToolUse", "session_id": "s1", "cwd": str(repo), "tool_name": "Read",
                      "tool_input": {"file_path": "/etc/hostname"}, "tool_response": {}})
                reload_view()
                settle()
            check("hooks that write no note do not make the card read its notes again", js("return window.__reads") == 0,
                  js("return window.__reads"))
            desk("POST", "/sessions/s1/line", {"text": "Read once when a note moves", "kind": "todo"}, token)
            reload_view()
            shown = until("return document.querySelector('.desk-notes').textContent.indexOf('Read once when a note moves') >= 0")
            settle()
            check("a new note makes the card read them once", bool(shown) and js("return window.__reads") == 1, js("return window.__reads"))
            js("window.__failReads = 1; return true")
            desk("POST", "/sessions/s1/line", {"text": "Read again after a failed read", "kind": "handoff"}, token)
            reload_view()
            until("return window.__reads >= 2")
            settle()
            reload_view()
            check("a failed read is made again on the next view",
                  until("return document.querySelector('.desk-notes').textContent.indexOf('Read again after a failed read') >= 0") is True,
                  js("return window.__reads"))
            # A line written before its session has an item is held, and lands
            # on the item with its own, older time when the session gets one.
            # The card reads its notes again then (desk review r2, finding 1).
            hook({"hook_event_name": "SessionStart", "session_id": "s2", "cwd": str(repo)})
            desk("POST", "/sessions/s2/line", {"text": "Held until its session has an item", "kind": "decision"}, token)
            time.sleep(0.05)
            desk("POST", "/sessions/s1/line", {"text": "Written after the held line", "kind": "finding"}, token)
            reload_view()
            until("return document.querySelector('.desk-notes').textContent.indexOf('Written after the held line') >= 0")
            desk("POST", "/sessions/s2/assign", {"item_id": item}, token)
            reload_view()
            check("a held line that lands when its session gets the item shows on the card",
                  until("return document.querySelector('.desk-notes').textContent.indexOf('Held until its session has an item') >= 0", 10) is True,
                  js("return document.querySelector('.desk-notes').textContent"))
            check("the pick-up list says why it is empty with no Linear connected",
                  "Linear" in js("return document.getElementById('desk-pickup-body').textContent"))
            if SHOT:
                Path(SHOT).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))

            js("document.querySelector('.desk-ok').click(); return true")
            settle()
            check("✓ on a tile nobody opened opens it instead of sending",
                  js("return !document.getElementById('desk-dialog').hidden"))
            check("the dialog shows the session's line",
                  "Agreed the bootload test plan" in js("return document.querySelector('.desk-lines').textContent"))
            if SHOT:
                Path(SHOT.replace(".png", "-dialog.png")).write_bytes(base64.b64decode(wd("GET", S + "/screenshot")))
            js("document.getElementById('desk-approve-big').click(); return true")
            settle()
            check("sending with no Harvest project says what is missing",
                  "Harvest project" in js("return document.getElementById('desk-dialog-status').textContent"),
                  js("return document.getElementById('desk-dialog-status').textContent"))
            js("document.getElementById('desk-dialog-close').click(); return true")

            js("window.confirm = function () { return true; }; document.querySelector('.desk-no').click(); return true")
            settle()
            check("✕ discards the tile and it leaves the hand", js("return document.querySelectorAll('.desk-tile').length") == 0)
            check("an empty hand says so", "No time waiting" in js("return document.getElementById('desk-hand').textContent"))

            desk("POST", "/items", {"title": "Pushed card"}, token)
            pushed = False
            for _ in range(70):
                if "Pushed card" in js("return document.getElementById('desk-cards').textContent"):
                    pushed = True
                    break
                time.sleep(0.2)
            check("a card made elsewhere arrives by SSE, no click and no client polling", pushed)
            check("desk.js holds no interval", "setInterval" not in (REPO / "assets" / "desk.js").read_text())

            # An agent's write is a draft on the tab, never sent on its own.
            desk("POST", "/drafts", {"kind": "linear_comment", "issue_key": "ABC-1", "body": "Flashed and passed."}, token)
            shown = False
            for _ in range(70):
                if "Comment on ABC-1" in js("return document.getElementById('desk-drafts').textContent"):
                    shown = True
                    break
                time.sleep(0.2)
            check("an agent's comment shows as a draft with Send and Discard", shown and
                  js("return !!document.querySelector('[data-draft-send]') && !!document.querySelector('[data-draft-discard]')"))
            js("document.querySelector('[data-draft-discard]').click(); return true")
            settle()
            check("Discard drops the draft and nothing is left to send", js("return document.getElementById('desk-drafts').hidden"))

            js("Array.from(document.querySelectorAll('.desk-card')).find(function (c) {"
               " return c.querySelector('.desk-card-title').textContent === 'Client sync call';"
               "}).querySelector('[data-timer-start]').click(); return true")
            settle()
            check("Start timer shows a running timer on the card's item",
                  "Client sync call" in js("return document.getElementById('desk-timer').textContent")
                  and js("return !!document.querySelector('.desk-card [data-timer-stop]')"))
            js("document.querySelector('#desk-timer [data-timer-stop]').click(); return true")
            settle()
            check("Stop ends it", js("return document.getElementById('desk-timer').hidden"))

            # --- desks: one per project, one open suite-wide -------------------
            (repo / "CMakeLists.txt").write_text("project(fw)\n")
            js("document.getElementById('desk-create').click(); return true")
            for _ in range(150):  # the form waits on Linear, Harvest and GitHub
                if js("return !!document.getElementById('df-name')"):
                    break
                time.sleep(0.2)
            js("document.getElementById('df-name').value = 'Bench client';"
               "document.getElementById('df-repo').value = arguments[0];"
               "document.querySelector('[data-form-save]').click(); return true", str(repo))
            opened = False
            for _ in range(60):
                if js("return document.getElementById('project-current-name').textContent") == "Bench client":
                    opened = True
                    break
                time.sleep(0.2)
            check("a desk made from the form opens, and the sidebar names it", opened,
                  js("return document.getElementById('project-current-name').textContent"))
            check("the Desk tab names the open desk", js("return document.getElementById('desk-head-name').textContent") == "Bench client")
            check("the first desk adopts the cards that came before it",
                  "Client sync call" in js("return document.getElementById('desk-cards').textContent"))
            day = js("return document.getElementById('desk-day-body').textContent")
            check("Today's work groups by desk and shows what was done",
                  "Bench client" in day and "Client sync call" in day and "Agreed the bootload test plan" in day, day[:200])
            js("document.getElementById('desk-new').value = 'Weekly call';"
               "document.getElementById('desk-add').click(); return true")
            settle()
            check("a standalone card lands on the desk picked for it (the open one by default)",
                  "Weekly call" in js("return document.getElementById('desk-cards').textContent"))

            # A switch made elsewhere (an agent, the CLI) reaches the page by itself.
            desk("POST", "/open", {"desk": "none"}, token)
            followed = False
            for _ in range(70):
                if js("return document.getElementById('project-current-path').textContent").startswith("no desk"):
                    followed = True
                    break
                time.sleep(0.2)
            check("closing the desk from outside the page reaches the sidebar", followed,
                  js("return document.getElementById('project-current-path').textContent"))
            check("with no desk open, no Desk notes strip is drawn", js("return document.getElementById('desk-dnotes').hidden") is True)
            ks = desk("GET", "/desks", token=token)["desks"]
            k = [d for d in ks if d["name"] == "Bench client"][0]
            desk("POST", "/desks", {"id": k["id"], "name": "Bench client", "firmware_repos": [str(repo)], "bench": "two-boards"}, token)
            desk("POST", "/open", {"desk": "Bench client"}, token)
            banner = False
            for _ in range(70):
                if "two-boards" in js("return document.getElementById('desk-bench-banner').textContent"):
                    banner = True
                    break
                time.sleep(0.2)
            check("opening a desk proposes its saved bench and touches nothing", banner and
                  js("return !!document.getElementById('desk-bench-review')"))
            js("document.getElementById('desk-bench-later').click(); return true")
            drive_desk_notes(js, until, settle, reload_view, real_click, type_into, replace_text, token, k["id"], repo, item, today)

            js("document.getElementById('desk-settings-toggle').click(); return true")
            settle()
            check("the Connectors panel opens with its add form", js("return !!document.getElementById('desk-conn-token')"))
            check("it offers Linear, Harvest and GitHub, and no Slack (embarch-desk decision 66)",
                  js("return Array.from(document.querySelectorAll('#desk-conn-kind option')).map(function (o) { return o.value; })")
                  == ["linear", "harvest", "github"]
                  and "slack" not in js("return document.getElementById('desk-settings').textContent.toLowerCase()"))
            # Linear MCP sign-in (embarch-desk decision 68): the link opens in
            # a tab and stays in the panel; the desk's answer ends the wait.
            # Registering at Linear is stubbed, so nothing leaves the machine.
            js("window.__opened = []; window.open = function (u) { window.__opened.push(u); return null; }; return true")
            js("var start = arguments[0], done = arguments[1];"
               "window.__realFetch = window.__realFetch || window.fetch;"
               "window.fetch = function (u) {"
               " var a = /\\/linear\\/signin$/.test(u) ? start : /\\/linear\\/signin\\/st1$/.test(u) ? done : null;"
               " if (a) return Promise.resolve(new Response(JSON.stringify(a), {status: 200}));"
               " return window.__realFetch.apply(window, arguments); }; return true",
               {"url": "https://mcp.linear.app/authorize?state=st1", "state": "st1"},
               {"status": "ok", "label": "client", "detail": "me @ Client, through Linear's MCP"})
            js("document.getElementById('desk-conn-label').value = 'client'; document.getElementById('desk-conn-mcp').click(); return true")
            said = until("return document.getElementById('desk-error').textContent.indexOf('Connected linear / client') >= 0", 8)
            check("signing in through Linear MCP opens Linear's page and reports the connection when the browser comes back",
                  said is True and js("return window.__opened[0]") == "https://mcp.linear.app/authorize?state=st1"
                  and js("return document.getElementById('desk-conn-signin').hidden") is True
                  and js("return document.getElementById('desk-conn-label').value") == "")
            unstub_fetch()
            js("document.getElementById('desk-conn-label').value = ''; document.getElementById('desk-conn-mcp').click(); return true")
            check("it asks for the workspace's name first",
                  until("return document.getElementById('desk-error').textContent.indexOf('name first') >= 0", 5) is True)
            # A save the desk refuses puts the box back and says why.
            was = js("return document.getElementById('desk-chat-panel').checked")
            stub_fetch("/api/desk/settings$", 500, "settings refused")
            js("document.getElementById('desk-chat-panel').click(); return true")
            said = until("return document.getElementById('desk-error').textContent.indexOf('settings refused') >= 0", 5)
            check("a refused save of the chat setting puts the box back and says why",
                  said is True and js("return document.getElementById('desk-chat-panel').checked") == was
                  and desk("GET", "/settings", token=token).get("chat_panel") == was)
            unstub_fetch()

            # Start work needs Linear, so the desk's answer is stubbed in the
            # page: with no Claude Code prompt link (the chat setting on) only
            # the folder opens; with one, the prompt link follows 5 s later.
            def start_with(answer):
                return wd("POST", S + "/execute/async", {"args": [answer, item], "script": """
                  var answer = arguments[0], item = arguments[1], done = arguments[2], delays = [];
                  var realFetch = window.fetch, realTimeout = window.setTimeout;
                  window.fetch = function (url) {
                    if (/\\/api\\/desk\\/items$/.test(url)) return Promise.resolve(new Response(JSON.stringify({ id: item })));
                    if (/\\/start$/.test(url)) return Promise.resolve(new Response(JSON.stringify(answer)));
                    return realFetch.apply(window, arguments);
                  };
                  window.setTimeout = function (f, ms) { delays.push(ms); return realTimeout(f, ms); };
                  document.getElementById('desk-error').innerHTML = '';
                  document.getElementById('desk-pickup-body').innerHTML =
                    '<div class="desk-pick"><button class="btn" type="button" data-pick-start="ABC-1">Start work</button></div>';
                  document.querySelector('[data-pick-start]').click();
                  (function wait(n) {
                    var text = document.getElementById('desk-error').textContent;
                    if (text.indexOf('Worktree') >= 0 || !n) {
                      window.fetch = realFetch; window.setTimeout = realTimeout;
                      done({ text: text, delays: delays });
                    } else realTimeout(function () { wait(n - 1); }, 100);
                  })(50);"""})
            start = {"worktree": "/w/fw-ABC-1", "branch": "abc-1-loader", "how": "reused", "moved_to": None, "warnings": [],
                     "folder_uri": "vscode://file/w/fw-ABC-1"}
            got = start_with(start)
            check("Start work with no prompt link opens only the folder, and says the chat starts there",
                  5000 not in got["delays"] and "its chat starts there" in got["text"], got)
            got = start_with({**start, "claude_uri": "vscode://anthropic.claude-code/open?prompt=x"})
            check("with a prompt link it opens the folder, then the prompt, as before",
                  5000 in got["delays"] and "press Enter there" in got["text"], got)

            # A card's Start work that the desk refuses (2026-10-26: pressed
            # on a card, nothing seen). The flash sat above the cards, out of
            # sight, and the next pushed view hid it within 10 s, so a refusal
            # read as nothing at all. The card itself says it now, under the
            # button, and a pushed view keeps both. Only an issue's card has
            # the button, and no connector is set up here, so it is put on
            # the scratch card in the page.
            card = '.desk-card[data-card="%s"]' % item
            js("var a = document.querySelector(arguments[0] + ' .desk-card-actions'), b = document.createElement('button');"
               "b.className = 'btn btn-tiny btn-primary'; b.type = 'button'; b.textContent = 'Start work';"
               "b.setAttribute('data-start', arguments[1]); a.prepend(b); b.scrollIntoView({block: 'center'});"
               "document.getElementById('desk-error').hidden = true; window.__startCard = a.closest('.desk-card'); return true",
               card, item)
            stub_fetch("/items/[^/]+/start$", 502, "the desk refused the start")
            real_click(card + " [data-start]")
            on_card = ("var s = document.querySelector(%s + ' [data-start-slot]'), r = s && s.getBoundingClientRect();"
                       "return !!s && s.textContent.indexOf('Start work failed: the desk refused the start') >= 0"
                       " && r.top >= 0 && r.bottom <= innerHeight") % json.dumps(card)
            check("a refused Start work says so on its card, under the button, in sight", until(on_card, 5) is True)
            pushed = until("return document.querySelector(%s) !== window.__startCard" % json.dumps(card), 15)
            flash_kept = js("var e = document.getElementById('desk-error');"
                            "return !e.hidden && e.textContent.indexOf('the desk refused the start') >= 0")
            check("the next pushed view keeps the refusal on the card and in the flash",
                  pushed is True and js(on_card) is True and flash_kept is True, [pushed, flash_kept])
            unstub_fetch()
            check("the Desk draws no Slack bar and no Slack panel (embarch-desk decision 65)",
                  js("return !document.getElementById('desk-slack-toggle') && !document.getElementById('desk-slack')"
                     " && !document.querySelector('.desk-slack-bar, .desk-msg')"))

            # The proxy carries the desk's token, so it answers only this UI's
            # own page (desk review r1, finding 2). A plain-text POST needs no
            # CORS preflight: from the UI's own page it reaches the desk as the
            # text it is, and the desk's JSON route refuses it.
            evil = "/w/from-elsewhere"
            got = wd("POST", S + "/execute/async", {"args": [evil], "script": """
              var done = arguments[1];
              fetch('/api/desk/mailbox', {method: 'POST', body: JSON.stringify({kind: 'focus', session_id: 'x1', folder: arguments[0]})})
                .then(function (r) { done(r.status); }, function (e) { done(String(e)); });"""})
            check("a plain-text body reaches the desk as text, and its JSON route refuses it",
                  got == 415 and mailbox_for(evil) == [], got)
            # From another origin (localhost is not 127.0.0.1), with no-cors.
            probe = desk("POST", "/items", {"title": "Cross-site probe", "desk": "Bench client"}, token)["id"]
            probe_tile = desk("PATCH", "/tiles/%s@%s" % (probe, today), {"hours": 0.25}, token)["id"]

            def tile_open(tid):
                try:
                    desk("PATCH", "/tiles/" + urllib.parse.quote(tid), {}, token)
                    return True
                except urllib.error.HTTPError as e:
                    return e.code
            wd("POST", S + "/url", {"url": "http://localhost:" + UI_PORT + "/"})
            wd("POST", S + "/execute/async", {"args": [UI, evil], "script": """
              var done = arguments[2];
              fetch(arguments[0] + '/api/desk/mailbox', {method: 'POST', mode: 'no-cors',
                    headers: {'Content-Type': 'text/plain'},
                    body: JSON.stringify({kind: 'focus', session_id: 'x1', folder: arguments[1]})})
                .then(function () { done(true); }, function () { done(true); });"""})
            check("a page on another origin cannot reach the desk through the proxy", mailbox_for(evil) == [])
            # A POST with no body has no Content-Type for the desk to refuse:
            # only the proxy's origin rule stops it (desk review r2, finding 4a).
            wd("POST", S + "/execute/async", {"args": [UI, probe_tile], "script": """
              var done = arguments[2];
              fetch(arguments[0] + '/api/desk/tiles/' + encodeURIComponent(arguments[1]) + '/discard', {method: 'POST', mode: 'no-cors'})
                .then(function () { done(true); }, function () { done(true); });"""})
            time.sleep(0.5)
            check("a bodyless POST from another origin cannot discard a tile through the proxy", tile_open(probe_tile) is True, tile_open(probe_tile))
            # Nor can it frame the UI's own page, whose requests would all be
            # same-origin (desk review r2, finding 3).
            loaded = wd("POST", S + "/execute/async", {"args": [UI + "/#desk"], "script": """
              var done = arguments[1], f = document.createElement('iframe');
              f.id = 'framed'; f.src = arguments[0]; f.onload = function () { done(true); };
              document.body.appendChild(f); setTimeout(function () { done(false); }, 8000);"""})
            wd("POST", S + "/frame", {"id": wd("POST", S + "/element", {"using": "css selector", "value": "#framed"})})
            framed = js("return {url: document.URL, desk: !!document.querySelector('.nav-item[data-tab=desk]')}")
            wd("POST", S + "/frame/parent", {})
            check("a page on another origin cannot frame the UI", loaded and not framed["desk"] and not framed["url"].startswith(UI), framed)
        finally:
            wd("DELETE", S)
        req = urllib.request.Request(UI + "/api/desk/mailbox", data=b'{"kind":"focus","session_id":"x1","folder":"/w/x"}', method="POST",
                                     headers={"Content-Type": "application/json", "Sec-Fetch-Site": "cross-site"})
        try:
            urllib.request.urlopen(req, timeout=5)
            check("a cross-site request to the proxy is refused", False)
        except urllib.error.HTTPError as e:
            check("a cross-site request to the proxy is refused", e.code == 403 and mailbox_for("/w/x") == [], e.code)

        # The real desk is down: the proxy says so in words, not a stack trace.
        procs[0].terminate()
        procs[0].wait(5)
        try:
            urllib.request.urlopen(UI + "/api/desk/view", timeout=5)
            check("a stopped desk is reported as stopped", False)
        except urllib.error.HTTPError as e:
            check("a stopped desk is reported as stopped", e.code == 503 and b"not running" in e.read())
    finally:
        for p in procs:
            if p.poll() is None:
                p.terminate()
        shutil.rmtree(scratch, ignore_errors=True)

    print("\n%d failed" % len(fails) if fails else "\nall passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
