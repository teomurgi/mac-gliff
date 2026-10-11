# Developing mac-gliff

How to build the client from source and test it. The design is in
`docs/architecture.md`; driver-specific behaviour is in
`docs/hardware-quirks.md`. The server side — capture, encode, input — is
developed upstream at [omacom/gliff](https://github.com/omacom/gliff) and
runs on the remote Linux machine.

## Build

```
bin/build
```

`bin/build` runs `cargo build --release --locked`, and the binary lands in
`target/release/`. The remote end of an SSH session still runs the
`gliff-server` installed there.

Needs Rust (`rust-toolchain.toml` pins a known-good compiler for rustup
users) and the GTK 4 libraries: `brew install pkg-config gtk4 libadwaita`.
A C++ toolchain is needed for the vendored OpenH264 build in `gliff-sw`
(nasm speeds it up).

`macos/build-dmg.sh` bundles the built client, with the whole GTK stack,
into `mac-gliff.app` and a DMG — see the README.

## Testing

- **`scripts/check.sh`** runs what CI runs (`cargo fmt --check`, `cargo
  clippy --all-targets -- -D warnings`, `cargo test --workspace`); run it
  before every push, and `scripts/check.sh --fix` to apply the rustfmt and
  clippy fixes first. Clippy lints gated on the MSRV (`rust-version` in
  `Cargo.toml`) switch on across the whole workspace when it is raised.
- **Unit tests** cover the pure logic: AVC444 split/recombine losslessness,
  single-stream subsample/upsample, the BGRA↔YUV colour round-trips and the
  AVX2 row kernels, the OpenH264 CPU-tier round-trip, message encoding
  stability and forward-compatibility, the H.264 access-unit split against
  an x264 stream, the macOS layout→xkb keymap selection and keycode
  mapping, and the clipboard rules and engine (mime filtering, URI lists,
  safe paths, the send window and assembler, chunked transfers between two
  engines, the size cap, and a spooled directory tree).
- **The hardware VideoToolbox test** needs a real Mac (CI runners only have
  software decode): `cargo test -p gliff-mtl -- --ignored`.
- **The GUI smoke** is an example rather than a test (AppKit requires the
  process main thread, which `#[test]` does not run on):
  `cargo run -p gliff --example glvideo` decodes the fixture through the
  client's widget; asserts delta 0.

## Release

The version is set in one place, `[workspace.package]` in `Cargo.toml`.

1. Set the version in `Cargo.toml`, then run `cargo build` to update
   `Cargo.lock`.
2. Commit the two files as "Release mac-gliff 0.2.0".
3. Tag the commit `v0.2.0` and push the commit and the tag.

CI runs the checks and builds the DMG, which is attached to the GitHub
release.
