// Focusing an embarch-ui tab that is already open, instead of opening a
// second one. The capability is a registry of topology strategies and
// exactly one of them is implemented (WSL + a Chromium browser on the
// Windows host); every other topology falls back to opening a tab, which
// is what this extension did unconditionally before (embarch-ui decision
// 28). A second topology is a new file plus one line of STRATEGIES here —
// not a refactor of extension.ts.

import * as cp from "child_process";
import * as fs from "fs";
import * as path from "path";

/** One way to find and raise an already-open browser tab.
 *
 * `supports()` answers "are we on the topology this strategy knows how to
 * drive?" and must never throw; `focus()` returns true **only** if a tab
 * was actually activated, so a false answer always means the caller should
 * open a tab the old way. Neither is ever allowed to be the reason nothing
 * opens: every failure mode — no browser window, no matching tab, a
 * missing helper binary, a timeout, a thrown error — resolves false. */
export interface FocusStrategy {
  id: string;
  supports(): Promise<boolean>;
  focus(title: string): Promise<boolean>;
}

/** Looks `name` up on PATH the way a shell would. Used instead of spawning
 * a probe process, so `supports()` costs nothing on the topologies that
 * don't have it. Deliberately not cached: renaming the helper away should
 * take effect on the next Start, not on the next window reload. */
function onPath(name: string): string | undefined {
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) {
      continue;
    }
    const candidate = path.join(dir, name);
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      // Not here; keep looking.
    }
  }
  return undefined;
}

/** The UI Automation script, with the tab title it matches on baked in.
 *
 * Four properties of this script are load-bearing; each was a real defect
 * found by measuring against the live browser before it was written:
 *
 * 1. `-cmatch 'Tab$'` is **case-sensitive**. A plain TabItem descendant
 *    search also returns the page's own in-page tab widgets — GitHub's
 *    `tabnav-tab`, Harvest's `pds-tab`, chess.com's `tabs-tab`, VS Code's
 *    own editor `tab` — which all end in lowercase `tab`. PowerShell's
 *    `-match` is case-insensitive and would let every one of them through,
 *    and clicking one clicks inside a page instead of switching tabs.
 * 2. **Every** window is enumerated, not `FindFirst`. The UIA root orders
 *    its children by z-order, so FindFirst returns whichever window is on
 *    top right now — and one browser process owned four windows here.
 *    Search first, raise second.
 * 3. The restore is **gated on `IsIconic`**. Calling `ShowWindow(RESTORE)`
 *    unconditionally un-maximizes a window that was maximized.
 * 4. The browser is never named. Chromium windows are found by their
 *    window class, which covers Brave, Chrome, Edge, Vivaldi and Opera
 *    without an allowlist to keep up to date. (Firefox is not Chromium and
 *    is not covered; it would be a second strategy, not a wider regex.)
 *
 * The UIA `Name` of a tab is not the document title — Brave appends its
 * own status annotations to it, e.g. `EmbArch - Memory usage - 22.3 MB` —
 * so the suffixes are stripped before the comparison, which is otherwise
 * exact. An exact match is why the page is titled just `EmbArch`: a
 * substring match on `embarch-ui` would also hit a GitHub tab or a local
 * file listing. */
function focusScript(title: string): string {
  const t = title.replace(/'/g, "''");
  return [
    "$ErrorActionPreference='Stop'",
    "Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes",
    "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices;" +
      ' public static class EmbArchWin {' +
      ' [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);' +
      ' [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);' +
      ' [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); }\'',
    "$AE=[System.Windows.Automation.AutomationElement]",
    "$TS=[System.Windows.Automation.TreeScope]",
    "$winCond=New-Object System.Windows.Automation.PropertyCondition($AE::ClassNameProperty,'Chrome_WidgetWin_1')",
    "$tabCond=New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty,[System.Windows.Automation.ControlType]::TabItem)",
    "foreach($w in $AE::RootElement.FindAll($TS::Children,$winCond)){",
    "  foreach($t in $w.FindAll($TS::Descendants,$tabCond)){",
    "    if($t.Current.ClassName -cmatch 'Tab$'){",
    "      $n=$t.Current.Name -replace ' - (Audio playing|Memory usage)\\b.*$',''",
    `      if($n -eq '${t}'){`,
    "        $t.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()",
    "        $h=[IntPtr]$w.Current.NativeWindowHandle",
    "        if([EmbArchWin]::IsIconic($h)){[void][EmbArchWin]::ShowWindow($h,9)}",
    "        [void][EmbArchWin]::SetForegroundWindow($h)",
    "        Write-Output 'FOCUSED'",
    "        exit 0",
    "      }",
    "    }",
    "  }",
    "}",
    "Write-Output 'NOTFOUND'",
  ].join("\n");
}

/** VS Code in WSL, browser on the Windows host, driven through
 * `powershell.exe` interop and Windows UI Automation.
 *
 * The script is passed **inline via `-Command`**, never as a `.ps1`: the
 * execution policy on a stock Windows install is Restricted, which fails
 * `-File` outright with UnauthorizedAccess but does not govern `-Command`.
 * That is why the script is a string constant in this file and the
 * extension ships no PowerShell file. */
export const wslChromiumUia: FocusStrategy = {
  id: "wsl-chromium-uia",

  async supports(): Promise<boolean> {
    return process.platform === "linux" && onPath("powershell.exe") !== undefined;
  },

  async focus(title: string): Promise<boolean> {
    const powershell = onPath("powershell.exe");
    if (!powershell) {
      return false;
    }
    return new Promise<boolean>((resolve) => {
      cp.execFile(
        powershell,
        ["-NoProfile", "-Command", focusScript(title)],
        { timeout: 4000 },
        (err, stdout) => {
          if (err) {
            resolve(false);
            return;
          }
          const lines = stdout.trim().split(/\r?\n/);
          resolve(lines[lines.length - 1]?.trim() === "FOCUSED");
        }
      );
    });
  },
};

const STRATEGIES: FocusStrategy[] = [wslChromiumUia];
// Unimplemented on purpose — each is a file, not a refactor:
//   linuxNativeAtSpi     VS Code on Linux, browser on the same X/Wayland session
//   windowsNativeUia     VS Code native on Windows (same UIA calls, no interop hop)
//   macOsAppleScript     `tell application "Google Chrome" to set active tab index`

/** Tries every strategy that claims this topology and returns true as soon
 * as one actually activated a tab. False means "nothing was focused" —
 * including the ordinary case of an unsupported topology, which is not an
 * error and never reaches the notification area. */
export async function focusExistingTab(
  title: string,
  log: (message: string) => void
): Promise<boolean> {
  let anySupported = false;
  for (const strategy of STRATEGIES) {
    let supported = false;
    try {
      supported = await strategy.supports();
    } catch {
      supported = false;
    }
    if (!supported) {
      continue;
    }
    anySupported = true;
    let focused = false;
    try {
      focused = await strategy.focus(title);
    } catch {
      focused = false;
    }
    log(`Focus strategy ${strategy.id}: ${focused ? "focused the existing tab" : "no matching tab"}`);
    if (focused) {
      return true;
    }
  }
  if (!anySupported) {
    log("Focus: no strategy supports this topology; opening a tab.");
  }
  return false;
}
