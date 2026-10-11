# Contributing to mac-gliff

Thanks for wanting to help. Bug reports, macOS quirks, and ports of upstream
features are all welcome.

## Getting set up

macOS (the client):

```sh
brew install pkg-config gtk4 libadwaita
cargo test -p gliff-mtl -p gliff-proto -p gliff  # should end green
```

Rust ≥ 1.92 — the gtk4-rs stack's floor; `rust-toolchain.toml` pins a
known-good compiler for rustup users. Use whatever manages your toolchain;
with [mise](https://mise.jdx.dev/) the local default may be older, so run
`mise x rust@latest -- cargo …`.

## Before you push

Run the checks CI runs:

```sh
scripts/check.sh            # fmt --check, clippy -D warnings, cargo test --workspace
scripts/check.sh --fix      # apply the rustfmt and clippy fixes first
```

On a real Mac, also run the hardware VideoToolbox test — CI runners only
have software decode:

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
| `crates/gliff-mtl/shaders/recombine.metal` | the MSL transliteration of upstream `gliff-vk`'s `recombine.comp` |
| `crates/gliff` | the vendored GTK client, with `cfg(target_os)` arms for the tiers |
| the other `crates/*` | the vendored protocol/transport/CPU-tier crates shared with upstream gliff (`gliff-proto`, `gliff-transport`, `gliff-sw`); the server-side crates live in the upstream repo |
| `docs/` | upstream architecture and quirks notes — read `docs/architecture.md` before touching the client |

## Conventions

- **Keep changes to the shared crates minimal.** The server crates are no
  longer vendored here; upstream `omacom/gliff` stays the reference for the
  protocol and the server behaviour. Keep changes to the shared crates
  (`gliff-proto`, `gliff-transport`, `gliff-sw`) minimal and
  protocol-compatible.
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
