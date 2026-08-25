# EmbArch UI Launcher

A thin VS Code extension: starts/stops the `embarch-ui` binary as a
subprocess and opens it in your system browser. It renders nothing inside
the editor — no webview, no custom panel — see
[embarch-ui/design.md §3 decision 3](../../embarch-doc/embarch-ui/design.md)
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

## Developing

```sh
npm install
npm run compile
```

Then press F5 in VS Code to launch an Extension Development Host with this
extension loaded.

**Not yet built/packaged in this environment** — the sandbox this extension
was authored in has no `node`/`npm` available (the same limitation noted in
[embarch-ui/design.md §5](../../embarch-doc/embarch-ui/design.md) for the
mockup canvas step). `npm install && npm run compile` needs to be run
somewhere with Node before this can be loaded into a real Extension
Development Host or packaged with `vsce`.

## Distribution

Not yet decided — see
[embarch-ui/design.md §5](../../embarch-doc/embarch-ui/design.md)
("VS Code extension distribution"). This extension is not published
anywhere; don't publish it without asking first.
