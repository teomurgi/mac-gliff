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

- [x] **macOS decode + compute**: `gliff-mtl::vt` (VideoToolbox session
  per stream, raw C FFI, hardware by default with a
  `new_software()` escape for virtual machines) and
  `gliff-mtl::compute` (Metal compute pipeline, `CVMetalTextureCache`
  zero-copy input, fast math off). The macOS CI runner decodes the
  recorded H.264 fixture through VideoToolbox, recombines it through
  the shader, and matches the CPU reference — the full pipeline, green.
- [x] **Wire into the client** (`crates/gliff`): a `VideoDecoder::Metal`
  arm behind `cfg(target_os = "macos")`, `gliff-vk` / `hypr-capture` /
  `hypr-input` (and the unused `hypr-ipc`, `drm-fourcc`) gated to Linux,
  and a `Frame::Metal` variant. The client builds and runs on macOS;
  decode is VT+Metal end to end with the CPU tier as fallback.
- [x] **Display, zero-copy**: the recombine writes into an IOSurface,
  and a `gtk::GLArea` video widget binds that same storage with
  `CGLTexImageIOSurface2D` (the `CVOpenGLTextureCache` plan does not
  apply — it wants a `CVImageBuffer`, not a bare IOSurface) and draws
  it through the paintable's layout math, so pointer mapping and zoom
  are identical across tiers. The `glvideo` example verifies the
  framebuffer against the decoded pixels: delta 0.
- [ ] **Input**: a kVK → evdev keycode table (both positional) plus
  shipping an xkb keymap derived from the active macOS input source.
- [ ] Packaging (Homebrew), codesigning.

## Layout

```
crates/gliff-mtl       the port: avcc + display (portable), vt + compute (macOS)
shaders/recombine.metal  the MSL transliteration of gliff-vk's recombine.comp
crates/gliff           the vendored client, wired to the tiers per OS
docs/                  upstream architecture and quirks notes
```

## Build and test

```sh
cargo test -p gliff-mtl          # portable core, any platform
cargo test -p gliff-mtl          # on macOS: also VideoToolbox + Metal,
                                 # including the shader-vs-reference check
cargo test -p gliff-mtl -- --ignored   # on a real Mac: the hardware-VT pipeline
cargo clippy -p gliff-mtl -p gliff --all-targets -- -D warnings
```

CI runs both on ubuntu (portable tests plus the client against GTK) and
macos-15 (the full tier plus the client). The client needs the GTK4
development libraries (`brew install pkg-config gtk4 libadwaita` on
macOS) and Rust ≥ 1.92, the floor of the locked gtk4-rs stack.
