"""The one place a drive script learns where geckodriver and the UI are.

Two variables, both a bare port on 127.0.0.1:
  EMBARCH_GECKO_PORT  geckodriver (default 4444, the owner's interactive one)
  EMBARCH_UI_PORT     the UI under test (default 4899, or the script's own)

A fleet worker sets both to scratch ports and never runs a script without
them. The older names (GECKO, UI, WEBDRIVER and the per-script *_UI_PORT)
are refused rather than ignored: a variable that does nothing is how a
script lands on someone else's browser. tests/drive_addresses.rs checks that
every drive*.py goes through this module."""
import os

LEGACY = ("GECKO", "UI", "WEBDRIVER", "ATLAS_UI_PORT", "LINKS_UI_PORT", "DESK_UI_PORT")


def _port(name, default):
    bad = [v for v in LEGACY if v in os.environ]
    if bad:
        raise SystemExit("%s no longer read; use EMBARCH_GECKO_PORT / EMBARCH_UI_PORT (tests/browser/README.md)"
                         % ", ".join(bad))
    value = os.environ.get(name, str(default))
    if not value.isdigit():
        raise SystemExit("%s must be a port number, got %r" % (name, value))
    return value


def gecko_port():
    return _port("EMBARCH_GECKO_PORT", 4444)


def ui_port(default=4899):
    return _port("EMBARCH_UI_PORT", default)


def gecko_url():
    return "http://127.0.0.1:" + gecko_port()


def ui_url(default=4899):
    return "http://127.0.0.1:" + ui_port(default)
