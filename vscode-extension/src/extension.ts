// EmbArch UI Launcher: a thin VS Code extension that starts/stops the
// `embarch-ui` binary as a subprocess and opens it in the system browser.
// Nothing renders inside the editor — no webview, no custom TreeView/editor
// (embarch-ui decision 3). This file is the whole extension.

import * as cp from "child_process";
import * as http from "http";
import * as vscode from "vscode";

import { focusExistingTab } from "./focus";

/** The `<title>` embarch-ui serves, and therefore the string the focus
 * strategies match a browser tab on — a load-bearing interface between two
 * components that otherwise don't know about each other (embarch-ui
 * decision 28). Renaming the page's title without changing this breaks
 * focus *silently*: Start keeps working, it just opens a duplicate tab
 * again, which is the original bug back with no error anywhere. */
const PAGE_TITLE = "EmbArch";

/** What the status bar is reporting. `external` — a server answering on
 * the configured address that this extension did not spawn — is knowable
 * only because `start()` now pre-flights the address; before that it was
 * indistinguishable from `stopped`. */
type ServerState = "stopped" | "ours" | "external";

/** Tracks the one subprocess this extension may have spawned. `undefined`
 * when embarch-ui isn't running under this extension's control — which
 * also covers the case where it's already running from somewhere else
 * (a terminal, another VS Code window): this extension only ever manages
 * a server it started itself, and `openInBrowser` works either way. */
let child: cp.ChildProcess | undefined;
let statusItem: vscode.StatusBarItem;
let output: vscode.OutputChannel;

function config() {
  const cfg = vscode.workspace.getConfiguration("embarchUi");
  return {
    binaryPath: cfg.get<string>("binaryPath", "embarch-ui"),
    host: cfg.get<string>("host", "127.0.0.1"),
    port: cfg.get<number>("port", 4890),
    configPath: cfg.get<string>("configPath", ""),
    autoStart: cfg.get<boolean>("autoStart", false),
  };
}

function serverUrl(): string {
  const { host, port } = config();
  return `http://${host}:${port}/`;
}

function setStatus(state: ServerState) {
  if (state === "ours") {
    statusItem.text = "$(server-process) embarch-ui";
    statusItem.tooltip = `embarch-ui running at ${serverUrl()} — click to stop`;
    statusItem.command = "embarchUi.stop";
    return;
  }
  if (state === "external") {
    statusItem.text = "$(server-process) embarch-ui (external)";
    // `stop()` can only kill a child this extension spawned, so an
    // external server's click goes to `start()` — which, finding the
    // address already answered, focuses its tab instead of adding one.
    statusItem.tooltip = `embarch-ui running at ${serverUrl()}, started outside this window — click to show it`;
    statusItem.command = "embarchUi.start";
    return;
  }
  statusItem.text = "$(server-process) embarch-ui (stopped)";
  statusItem.tooltip = "embarch-ui is not running — click to start";
  statusItem.command = "embarchUi.start";
}

/** Is something already serving the configured address? A 400 ms GET that
 * resolves true on *any* response — the question is whether the address is
 * answered, not what it answers. Loopback, so a real server replies in
 * single-digit milliseconds and the timeout only ever covers a dropped
 * packet. */
function probe(): Promise<boolean> {
  const { host, port } = config();
  return new Promise<boolean>((resolve) => {
    const req = http.get({ host, port, path: "/", timeout: 400 }, (res) => {
      res.resume();
      resolve(true);
    });
    req.on("timeout", () => {
      req.destroy();
      resolve(false);
    });
    req.on("error", () => resolve(false));
  });
}

/** Show the running server: focus the tab that already has it open, and
 * only open a new one if there is no such tab. Every `start()` path goes
 * through here; `embarchUi.openInBrowser` deliberately does not, so there
 * is always an unconditional way to get a second tab. */
async function reveal(): Promise<void> {
  if (await focusExistingTab(PAGE_TITLE, (message) => output.appendLine(message))) {
    return;
  }
  await openInBrowser();
}

/** Starts embarch-ui unless something is already serving its address, then
 * shows it — focusing the tab that already has it open rather than opening
 * another one.
 *
 * The address is pre-flighted before spawning, so a server started some
 * other way (a terminal, another VS Code window) is recognised as such
 * instead of being inferred from a failed bind after the fact. That
 * inference is kept below as a backstop for the race where something binds
 * the port between the probe and the spawn. */
async function start(): Promise<void> {
  if (child) {
    await reveal();
    return;
  }

  if (await probe()) {
    output.appendLine(`embarch-ui is already serving ${serverUrl()} — not spawning another.`);
    setStatus("external");
    await reveal();
    return;
  }

  const { binaryPath, configPath, host, port } = config();
  const env = { ...process.env };
  // Forward the configured host/port to the binary. Without this the two
  // disagreed: the binary bound its own hardcoded 127.0.0.1:4890 while
  // `serverUrl()` above built the URL to open from these settings, so any
  // non-default value opened a browser at an address nothing was serving.
  env.EMBARCH_UI_HOST = host;
  env.EMBARCH_UI_PORT = String(port);
  if (configPath) {
    env.EMBARCH_UI_CONFIG = configPath;
  }

  output.appendLine(`Starting: ${binaryPath}`);
  const proc = cp.spawn(binaryPath, [], { env });
  child = proc;
  setStatus("ours");

  let settled = false;
  let sawListening = false;

  const onLine = (data: Buffer) => {
    const text = data.toString();
    output.append(text);
    if (!sawListening && text.includes("listening on")) {
      sawListening = true;
    }
  };
  proc.stdout?.on("data", onLine);
  proc.stderr?.on("data", onLine);

  proc.on("error", (err) => {
    output.appendLine(`Failed to start embarch-ui: ${err.message}`);
    vscode.window.showErrorMessage(
      `Failed to start embarch-ui (binary: "${binaryPath}"). Set "embarchUi.binaryPath" if it isn't on PATH. ${err.message}`
    );
    if (child === proc) {
      child = undefined;
      setStatus("stopped");
    }
    settled = true;
  });

  proc.on("exit", (code, signal) => {
    output.appendLine(`embarch-ui exited (code=${code ?? "null"}, signal=${signal ?? "null"})`);
    if (child === proc) {
      child = undefined;
      setStatus("stopped");
    }
    // A near-immediate exit before we ever saw "listening on" most likely
    // means the port is already taken by another embarch-ui instance —
    // not a real failure from this extension's point of view. The probe
    // above catches this case first now; this stays for the race where
    // something bound the port in between.
    if (!settled && !sawListening) {
      settled = true;
      setStatus("external");
      void reveal();
    }
  });

  // Don't block the command on a fixed sleep: open as soon as we see the
  // real "listening on" log line, falling back to a short timeout in case
  // the binary's own logging ever changes shape.
  await new Promise<void>((resolve) => {
    const timeout = setTimeout(() => resolve(), 3000);
    const check = setInterval(() => {
      if (sawListening || settled) {
        clearInterval(check);
        clearTimeout(timeout);
        resolve();
      }
    }, 100);
  });
  settled = true;

  if (child) {
    await reveal();
  }
}

async function stop(): Promise<void> {
  if (!child) {
    vscode.window.showInformationMessage("embarch-ui is not running (or was started outside this extension).");
    return;
  }
  output.appendLine("Stopping embarch-ui.");
  child.kill();
  child = undefined;
  setStatus("stopped");
}

async function openInBrowser(): Promise<void> {
  await vscode.env.openExternal(vscode.Uri.parse(serverUrl()));
}

export function activate(context: vscode.ExtensionContext): void {
  output = vscode.window.createOutputChannel("EmbArch UI");
  statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  setStatus("stopped");
  statusItem.show();

  context.subscriptions.push(
    output,
    statusItem,
    vscode.commands.registerCommand("embarchUi.start", start),
    vscode.commands.registerCommand("embarchUi.stop", stop),
    vscode.commands.registerCommand("embarchUi.openInBrowser", openInBrowser)
  );

  if (config().autoStart) {
    void start();
  }
}

/** Stops a subprocess this extension spawned when VS Code closes — never
 * leave an orphaned embarch-ui running past the editor session that
 * started it. A server started outside this extension (`child` unset) is
 * left alone, matching the same "only manage what we started" rule as
 * `start()`/`stop()` above. */
export function deactivate(): void {
  if (child) {
    child.kill();
    child = undefined;
  }
}
