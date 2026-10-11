# Contributing to mac-gliff

Thanks for wanting to help. Bug reports, macOS quirks, and ports of upstream
features are all welcome.

## Getting set up

macOS (the client and the port):

```sh
brew install pkg-config gtk4 libadwaita
cargo test -p gliff-mtl -p gliff     # should end green
```

Rust ≥ 1.92 — the gtk4-rs stack's floor. Use whatever manages your toolchain;
with [mise](https://mise.jdx.dev/) the local default may be older, so run
`mise x rust@latest -- cargo …`.

Linux (the server, the Linux client, `gliff-probe`): the CI set is
`libgtk-4-dev libadwaita-1-dev libva-dev libgbm-dev libdrm-dev
libxkbcommon-dev libvulkan-dev nasm pkg-config`.

## Before you push

On Linux, run the whole thing:

```sh
scripts/check.sh            # fmt --check, clippy -D warnings, cargo test --workspace
scripts/check.sh --fix      # apply the rustfmt and clippy fixes first
```

On macOS, the workspace build stops at the server crates (they need Linux
libraries — `gliff-server` links dbus), so run the same subset CI runs there:

```sh
cargo fmt --all --check
cargo clippy -p gliff-mtl -p gliff --all-targets -- -D warnings
cargo test -p gliff-mtl -p gliff
```

CI runs that subset on `macos-15` and the Linux equivalents on
`ubuntu-latest`; a pull request must be green on both. On a real Mac, also run
the hardware VideoToolbox test — CI runners only have software decode:

```sh
cargo test -p gliff-mtl -- --ignored
```

The GUI smoke is an example rather than a test (AppKit requires the process
main thread, which `#[test]` does not run on):

```sh
cargo run -p gliff --example glvideo   # decodes the fixture through the
                                       # client's widget; asserts delta 0
```

## Layout

| Path | What it is |
|---|---|
| `crates/gliff-mtl` | the port: `avcc`/`display` (portable core), `vt` (VideoToolbox), `compute` (Metal), `client` (the tier facade) |
| `crates/gliff-mtl/shaders/recombine.metal` | the MSL transliteration of `gliff-vk`'s `recombine.comp` |
| `crates/gliff` | the vendored GTK client, with `cfg(target_os)` arms for the tiers |
| the other `crates/*` | vendored upstream gliff: server, protocol, transport, Linux tiers |
| `docs/` | upstream architecture and quirks notes — read `docs/architecture.md` before touching the client |

## Conventions

- **Keep port commits mechanical.** Upstream `omacom/gliff` is vendored
  in-tree; keep the macOS changes small and `cfg`-scoped, with macOS-only
  dependencies under `[target.'cfg(target_os = "macos")'.dependencies]`, so
  upstream updates keep rebasing cleanly.
- **`unsafe` blocks carry a `// SAFETY:` comment** — the workspace lints
  (`undocumented_unsafe_blocks`, `unsafe_op_in_unsafe_fn`) are kept clean.
- **GPU code is oracle-tested**: VideoToolbox + Metal output is checked
  against the CPU reference (`gliff-mtl`'s tests); keep new GPU paths under
  the same checks.
- Commits: `<crate>: what changed` — e.g. `gliff-mtl: …`, `gliff: …`.

## Reporting a bug

Open an issue with the macOS version, whether it is a real Mac or a virtual
machine, the output with `RUST_LOG=debug`, and whether `--video cpu` changes
it. `--listen`/`--connect` are development-only transports with no
authentication — do not expose them.
