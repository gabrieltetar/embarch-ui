# EmbArch UI Launcher

A thin VS Code extension: starts/stops the `embarch-ui` binary as a
subprocess and opens it in your system browser. It renders nothing inside
the editor — no webview, no custom panel — see
[embarch-ui decision 3](../../embarch-doc/embarch-ui/decisions/shape.md)
for why.

## Commands

- **EmbArch UI: Start** — spawns `embarch-ui` (if not already running under
  this extension) and opens it in your browser.
- **EmbArch UI: Stop** — stops the subprocess this extension started.
- **EmbArch UI: Open in Browser** — just opens the browser at embarch-ui's
  bound address, without starting anything. Useful if embarch-ui is already
  running some other way (a terminal, another VS Code window).

A status bar item ("embarch-ui" / "embarch-ui (stopped)") mirrors the
current state and toggles start/stop when clicked.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `embarchUi.binaryPath` | `"embarch-ui"` | Path to the binary. Set an absolute path if it isn't on `PATH`. |
| `embarchUi.host` | `"127.0.0.1"` | Must match embarch-ui's own bind address. |
| `embarchUi.port` | `4890` | Must match embarch-ui's own bind port. |
| `embarchUi.configPath` | `""` (unset) | Optional path to an embarch-ui TOML config file; sets `EMBARCH_UI_CONFIG` for the spawned process. |
| `embarchUi.autoStart` | `false` | Start embarch-ui when VS Code starts, stop it when VS Code closes. |

## Installing

```sh
./reinstall.sh
```

Compiles if the build is stale, packages the `.vsix`, sideloads it into the
VS Code server, and tells you to reload the window — the commands and the
status bar item only appear after a reload. Safe to re-run at any time.

**You will need it again.** The launcher is not on any marketplace, so
nothing reinstalls it: when the VS Code server re-provisions its extensions
directory — a server update does — the extension is dropped silently while
the `embarchUi.*` settings survive, so the symptom is the three commands
quietly missing from the palette with no error anywhere.

The script depends on nothing outside the box, because this bench has no
`node`, `npm` or `vsce` on `PATH`: it borrows the `node` bundled inside the
VS Code server, and builds the `.vsix` with python's `zipfile` instead of
with `vsce` (a `.vsix` is only a zip holding a manifest and the extension
directory). It installs into the **WSL remote**, not Windows, which is where
the extension has to run to spawn the Linux `embarch-ui` binary.

## Developing

```sh
npm install
npm run compile
```

Then press F5 in VS Code to launch an Extension Development Host with this
extension loaded — which needs a real `node`/`npm`, and, if this box still
has neither, is exactly the path `reinstall.sh` exists to avoid. `out/` and
`node_modules/` are both gitignored, so a fresh clone cannot compile here at
all; `reinstall.sh` says so plainly rather than failing obscurely.

## Distribution

**An internal `.vsix` only, never the public Marketplace** —
[embarch-ui decision 3](../../embarch-doc/embarch-ui/decisions/shape.md).
This extension is not published anywhere; don't publish it without asking
first.
