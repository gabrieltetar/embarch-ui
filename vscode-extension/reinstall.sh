#!/usr/bin/env bash
# Rebuild this extension and sideload it back into VS Code.
#
# The launcher is not on the marketplace, so nothing reinstalls it: when the
# VS Code server re-provisions its extensions directory (a server update will),
# the extension is dropped silently — the three "EmbArch UI:" commands just stop
# appearing in the palette, while the embarchUi.* settings survive. This script
# is the recovery path, and is safe to re-run at any time.
#
# It deliberately depends on nothing outside the box: WSL here has no node, npm
# or vsce on PATH, so it borrows the node bundled inside the VS Code server and
# builds the .vsix with python's zipfile rather than with vsce. A .vsix is only
# a zip holding a manifest and the extension directory.
set -euo pipefail
cd "$(dirname "$0")"

say() { printf '%s\n' "$*"; }
die() { printf 'reinstall.sh: %s\n' "$*" >&2; exit 1; }

# --- compile, but only if the build is actually stale ---------------------
# out/ is gitignored, so a fresh clone has to compile; an existing tree
# usually doesn't, which matters because compiling is the one step that
# needs node_modules and node_modules needs an npm this box may not have.
needs_compile=false
if [[ ! -f out/extension.js ]]; then
  needs_compile=true
else
  for f in src/*.ts tsconfig.json; do
    [[ -e "$f" && "$f" -nt out/extension.js ]] && needs_compile=true
  done
fi

if [[ "$needs_compile" == true ]]; then
  node_bin="$(command -v node || true)"
  if [[ -z "$node_bin" ]]; then
    # The VS Code server ships its own node. Newest first, in case several
    # server versions are still unpacked under ~/.vscode-server/bin/.
    node_bin="$(ls -t "$HOME"/.vscode-server/bin/*/node 2>/dev/null | head -1 || true)"
  fi
  [[ -n "$node_bin" ]] || die "no node found (not on PATH, none bundled under ~/.vscode-server/bin/)"
  [[ -x node_modules/.bin/tsc ]] || die "out/extension.js is stale but node_modules/ is missing — run 'npm install' here (needs a real node/npm, which this WSL does not have) or copy node_modules/ in"

  say "Compiling with $node_bin"
  "$node_bin" node_modules/.bin/tsc -p .
else
  say "out/extension.js is current — skipping compile"
fi

# --- package -------------------------------------------------------------
# ExtensionKind is pinned to "workspace" on purpose: the extension spawns the
# Linux embarch-ui binary, so it has to run on the remote (WSL) side, not on
# the Windows UI side.
vsix="$(python3 - <<'PY'
import json, zipfile
from xml.sax.saxutils import quoteattr, escape

pkg = json.load(open("package.json"))
repo = pkg.get("repository", {}).get("url", "")
vsix = f"{pkg['name']}-{pkg['version']}.vsix"

manifest = f"""<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id={quoteattr(pkg['name'])} Version={quoteattr(pkg['version'])} Publisher={quoteattr(pkg['publisher'])} />
    <DisplayName>{escape(pkg['displayName'])}</DisplayName>
    <Description xml:space="preserve">{escape(pkg['description'])}</Description>
    <Tags></Tags>
    <Categories>{escape(','.join(pkg.get('categories', ['Other'])))}</Categories>
    <GalleryFlags>Public</GalleryFlags>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value={quoteattr(pkg['engines']['vscode'])} />
      <Property Id="Microsoft.VisualStudio.Code.ExtensionKind" Value="workspace" />
      <Property Id="Microsoft.VisualStudio.Code.ExecutesCode" Value="true" />
      <Property Id="Microsoft.VisualStudio.Services.Links.Source" Value={quoteattr(repo)} />
      <Property Id="Microsoft.VisualStudio.Services.GitHubFlavoredMarkdown" Value="true" />
      <Property Id="Microsoft.VisualStudio.Services.Content.Pricing" Value="Free" />
    </Properties>
    <License>extension/LICENSE.txt</License>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code" />
  </Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true" />
    <Asset Type="Microsoft.VisualStudio.Services.Content.Details" Path="extension/readme.md" Addressable="true" />
    <Asset Type="Microsoft.VisualStudio.Services.Content.License" Path="extension/LICENSE.txt" Addressable="true" />
  </Assets>
</PackageManifest>
"""

content_types = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension=".js" ContentType="application/javascript"/><Default Extension=".json" ContentType="application/json"/><Default Extension=".md" ContentType="text/markdown"/><Default Extension=".txt" ContentType="text/plain"/><Default Extension=".vsixmanifest" ContentType="text/xml"/></Types>
"""

with zipfile.ZipFile(vsix, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("extension.vsixmanifest", manifest)
    z.writestr("[Content_Types].xml", content_types)
    z.writestr("extension/package.json", open("package.json", "rb").read())
    z.writestr("extension/out/extension.js", open("out/extension.js", "rb").read())
    z.writestr("extension/readme.md", open("README.md", "rb").read())
    z.writestr("extension/LICENSE.txt", open("LICENSE", "rb").read())

print(vsix)
PY
)"
say "Packaged $vsix"

# --- install -------------------------------------------------------------
# Inside VS Code's integrated terminal `code` is already the remote CLI; from
# a plain shell it usually isn't on PATH at all, hence the fallback.
code_bin="$(command -v code || true)"
if [[ -z "$code_bin" ]]; then
  code_bin="$(ls -t "$HOME"/.vscode-server/bin/*/bin/remote-cli/code 2>/dev/null | head -1 || true)"
fi
[[ -n "$code_bin" ]] || die "no VS Code CLI found (not on PATH, none under ~/.vscode-server/bin/*/bin/remote-cli/) — install by hand with: Extensions view → ... → Install from VSIX"

say "Installing with $code_bin"
"$code_bin" --install-extension "./$vsix" --force

say
say "Installed. Reload the VS Code window (Developer: Reload Window) — the"
say "'EmbArch UI: Start / Stop / Open in Browser' commands and the status bar"
say "item only appear after a reload."
