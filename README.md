# mac-gliff

A macOS port of the [gliff](https://github.com/omacom/gliff) remote
desktop client: VideoToolbox H.264 decode and a Metal compute recombine,
replacing the Linux client's VA-API + Vulkan GPU tier. The goal is the
same sharp 4:4:4 text gliff delivers on Linux — hardware decode, no
pixel copies — between a Mac and a Hyprland machine over ssh.

Derived from omacom/gliff @ a2edd50 (MIT); the vendored upstream crates
are kept in place so the port can be wired into the client with the
smallest possible diff. See `docs/` for the upstream architecture notes
that still apply.

## Status

- [x] **Portable core, tested everywhere**: `gliff-mtl::avcc` (Annex B →
  AVCC framing, parameter-set extraction) and `gliff-mtl::display` (the
  CPU recombine reference), both checked against the `gliff-proto`
  oracle.
- [x] **macOS decode + compute**: `gliff-mtl::vt` (VideoToolbox session
  per stream, raw C FFI) and `gliff-mtl::compute` (Metal compute
  pipeline, `CVMetalTextureCache` zero-copy input, fast math off).
  Built and exercised on the macOS CI runner, which decodes the
  recorded H.264 fixture through VideoToolbox, recombines it through
  the shader, and compares against the reference.
- [ ] **Wire into the client** (`crates/gliff`): a `VideoDecoder::Metal`
  arm behind `cfg(target_os = "macos")`, cfg-gating `gliff-vk` /
  `hypr-capture` / `hypr-input` off darwin, and a `Frame` variant for
  the Metal output.
- [ ] **Display**: the video widget on macOS — GTK4 has no IOSurface
  path; the plan is `gtk::GLArea` + `CVOpenGLTextureCache` sharing the
  same IOSurface Metal writes.
- [ ] **Input**: a kVK → evdev keycode table (both positional) plus
  shipping an xkb keymap derived from the active macOS input source.
- [ ] Packaging (Homebrew), codesigning.

## Layout

```
crates/gliff-mtl       the port: avcc + display (portable), vt + compute (macOS)
shaders/recombine.metal  the MSL transliteration of gliff-vk's recombine.comp
crates/...             vendored gliff, unmodified so far
docs/                  upstream architecture and quirks notes
```

## Build and test

```sh
cargo test -p gliff-mtl          # portable core, any platform
cargo test -p gliff-mtl          # on macOS: also VideoToolbox + Metal,
                                 # including the shader-vs-reference check
cargo clippy -p gliff-mtl --all-targets -- -D warnings
```

CI runs both on ubuntu (portable tests) and macos-15 (the full tier).
