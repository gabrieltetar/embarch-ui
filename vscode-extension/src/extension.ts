// EmbArch UI Launcher: a thin VS Code extension that starts/stops the
// `embarch-ui` binary as a subprocess and opens it in the system browser.
// Nothing renders inside the editor — no webview, no custom TreeView/editor
// (embarch-ui/design.md §3 decision 3). This file is the whole extension.

import * as cp from "child_process";
import * as vscode from "vscode";

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

function setStatus(running: boolean) {
  statusItem.text = running ? "$(server-process) embarch-ui" : "$(server-process) embarch-ui (stopped)";
  statusItem.tooltip = running
    ? `embarch-ui running at ${serverUrl()} — click to stop`
    : "embarch-ui is not running — click to start";
  statusItem.command = running ? "embarchUi.stop" : "embarchUi.start";
}

/** Starts embarch-ui if this extension doesn't already have a handle on a
 * running instance, then opens the system browser at its bound address.
 * If embarch-ui is already running some other way (a terminal, another VS
 * Code window), spawning here will fail to bind the port — that failure is
 * treated as "someone else is already serving this address" and the
 * browser opens anyway, rather than surfacing a scary error for a state
 * that's actually fine. */
async function start(): Promise<void> {
  if (child) {
    await openInBrowser();
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
  setStatus(true);

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
      setStatus(false);
    }
    settled = true;
  });

  proc.on("exit", (code, signal) => {
    output.appendLine(`embarch-ui exited (code=${code ?? "null"}, signal=${signal ?? "null"})`);
    if (child === proc) {
      child = undefined;
      setStatus(false);
    }
    // A near-immediate exit before we ever saw "listening on" most likely
    // means the port is already taken by another embarch-ui instance —
    // not a real failure from this extension's point of view.
    if (!settled && !sawListening) {
      settled = true;
      openInBrowser();
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
    await openInBrowser();
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
  setStatus(false);
}

async function openInBrowser(): Promise<void> {
  await vscode.env.openExternal(vscode.Uri.parse(serverUrl()));
}

export function activate(context: vscode.ExtensionContext): void {
  output = vscode.window.createOutputChannel("EmbArch UI");
  statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  setStatus(false);
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
