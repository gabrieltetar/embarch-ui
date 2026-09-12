# embarch-ui

Part of the [EmbArch](https://github.com/gabrieltetar/embarch-doc) suite — a set of tools for firmware engineers that spans from software to the physical hardware bench.

`embarch-ui` is the one place a firmware engineer looks to exercise suite features **by hand** — day to day, not only when an agent needs a hardware read. It is a single Rust binary that serves a local web app on `http://127.0.0.1:4890` and talks to [`embarch-core`](https://github.com/gabrieltetar/embarch-core) over HTTP+Bearer for everything hardware-adjacent.

It replaced three separate ad hoc UI surfaces outright rather than adding a fourth: `embarch-topology`'s read-only board view, `embarch-study-designer`'s standalone study builder, and Core's own enroll page. All three are retired.

It is **not** a build-toolchain project, not a VS Code webview, not a second owner of hardware mutation, and not a replacement for [`embarch-api`](https://github.com/gabrieltetar/embarch-api)'s MCP/CLI surface — agents keep talking to `embarch-api`.

## The six tabs

One persistent left sidebar, one top status bar, client-side navigation by URL fragment (`#topology`, `#trace?study=<id>&tap=<name>`).

| Tab | What it does |
|---|---|
| **Dashboard** | Active study and alert cards, live |
| **Topology** | The board/probe diagram, the alert list, and signal routing — the one human surface for declaring a DUT signal's route |
| **Study Designer** | Authoring a study: steps, the two `requires` fields, security levels, GATT capture taps, the declared-GATT picker, opening a firmware project |
| **Enroll** | Submits to Core's enroll endpoint |
| **Trace** | Renders a completed study's outpost capture: lanes, gap bands, a load repartition, a study-step row |
| **Debug** | Live log tail, switchable between Core and `embarch-api` |

Everything live reaches the browser as **SSE** served by this binary — there is no client-side interval polling anywhere. Where a source has no push surface this repo consumes, the polling is server-side and the browser never sees it.

## Shape

```
embarch-ui (one Rust binary, axum, zero-build — no bundler, assets include_str!-embedded)
  |
  +-- links embarch-study-designer  in-process: merged action list, custom-action
  |                                 registry, study building, outpost trace decode
  |                                 — pure data, no I/O
  +-- links embarch-core-client     the one implementation of "reach Core over
  |                                 HTTP+Bearer", shared with embarch-api
  |
  +-- HTTP + Bearer --> embarch-core --probe-rs/serialport--> hardware

vscode-extension/ (thin, TypeScript)
  spawns/stops the binary, opens the system browser, renders nothing itself
```

**Every hardware-adjacent call, read or write, goes over HTTP+Bearer to Core**, and that is structural rather than a convention: `embarch-topology` is in the tree transitively via `embarch-core-client`, but only its `software` feature — never `hardware` — so neither `probe-rs` nor `serialport` appears in `cargo tree -e normal`. A board read done in-process would enumerate whichever machine `embarch-ui` runs on, not Core's.

## Running it

Requires a running [`embarch-core`](https://github.com/gabrieltetar/embarch-core).

**And this repo does not build on its own.** It has the deepest sibling reach in the
suite — three repos must be cloned into the same parent directory, because they are
depended on by relative path:

| Sibling | Why |
|---|---|
| [`embarch-study-designer`](https://github.com/gabrieltetar/embarch-study-designer) | the shared study/registry type model, named by this repo's own `Cargo.toml` |
| [`embarch-api`](https://github.com/gabrieltetar/embarch-api) | **not for itself** — for `crates/embarch-core-client`, a crate that lives *inside* that repo |
| [`embarch-topology`](https://github.com/gabrieltetar/embarch-topology) | named by neither manifest you would think to read: `embarch-core-client` depends on it, so it is needed transitively |

The last two are the trap. Reading this repo's `Cargo.toml` alone gives you a checkout
that still fails its first build, with `failed to read
.../embarch-topology/Cargo.toml` — an error naming a path outside this repo, which
means nothing more than "the sibling is not there".

So the layout cargo expects is `<parent>/embarch-ui`, `<parent>/embarch-study-designer`,
`<parent>/embarch-api`, `<parent>/embarch-topology`.

Path dependencies rather than git or registry ones is a deliberate choice, not an
oversight: `embarch-study-designer` decision 8 and `embarch-topology` decision 13.

```sh
cargo run --release      # then open http://127.0.0.1:4890
```

No CLI flags — the whole surface is four environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `EMBARCH_UI_HOST` | `127.0.0.1` | Bind address. Loopback-only by default: no TLS, and no reason to expose a tool one engineer runs on their own machine. |
| `EMBARCH_UI_PORT` | `4890` | Bind port. An unparseable value falls back to the default rather than refusing to start; the address actually bound is logged on every start. |
| `EMBARCH_UI_CONFIG` | unset | Path to an optional TOML config file (below). |
| `EMBARCH_UI_STATE` | unset | Path to the recent-projects list, which otherwise lives at `<per-user data dir>/embarch/ui/recent-projects.json`. **Not the config file** — that one an engineer writes and this process only reads; process-written state goes to the per-user data dir instead. An unreadable or unparseable file is an empty list, logged, never an error. |

## Config

**None is needed for the common single-machine case.** Without `EMBARCH_UI_CONFIG` the defaults are `base_url = "auto"` — resolve Core at first use, because the WSL2 host-gateway address changes on every WSL restart and any literal IP goes stale — and token discovery falling through to the machine-wide token file `embarch-core` generates.

The file, when you want one:

```toml
[core]                       # identical schema to embarch-api's own [core]:
base_url = "auto"            # both are the same embarch_core_client::CoreConfig
# host = "192.168.1.50"      # only consulted by base_url = "auto", as its last candidate
# port = 8080
# token_env = "EMBARCH_TOKEN"

[study_designer]             # optional: the zero-click default for a single-repo bench.
firmware_repo_path = "/path/to/firmware"   # its embarch/study-actions.toml is the registry
# static_extractor = "zephyr-ble-def"      # absent, static GATT extraction is skipped
```

Omitting `[study_designer]` does not disable the tab — "Open project" can pick a firmware repo at runtime.

## The VS Code launcher

[`vscode-extension/`](vscode-extension) is a thin launcher: it starts and stops the binary and opens your system browser. It renders nothing inside the editor. Install it with `./reinstall.sh` from that directory, then reload the window.

**You will need that script again.** The launcher is not on any marketplace, so nothing reinstalls it: when the VS Code server re-provisions its extensions directory — a server update does — the extension is dropped silently while the `embarchUi.*` settings survive. The symptom is the three "EmbArch UI:" commands quietly missing from the palette, with no error anywhere. See [vscode-extension/README.md](vscode-extension/README.md).

## Testing

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

`tests/element_ids.rs` is a static guard over the id surface of `assets/index.html` and `assets/app.js`: no id declared twice, and no `getElementById`/`sdEl`/`trEl`/`sigEl` lookup dangling. It exists because "tested in Rust, never looked at" is how this repo's worst defects hid — the two instruments that catch the rest of that shape (a headless-Firefox harness over `app.js`, and driving the deployed binary with real clicks) are described under *Verification technique* in the spec. The assets are `include_str!`-embedded, so for anything that depends on them the deployed artifact is the only thing that can be checked.

## Design doc

Current truth: [`embarch-doc/embarch-ui/spec.md`](https://github.com/gabrieltetar/embarch-doc/blob/main/embarch-ui/spec.md) — including the invariants this UI holds itself to. Why it is that way: [`decisions.md`](https://github.com/gabrieltetar/embarch-doc/blob/main/embarch-ui/decisions.md). Unresolved: [`open.md`](https://github.com/gabrieltetar/embarch-doc/blob/main/embarch-ui/open.md). Reference: [`interfaces.md`](https://github.com/gabrieltetar/embarch-doc/blob/main/embarch-ui/interfaces.md).

## License

MIT — see [LICENSE](LICENSE).
