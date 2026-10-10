# mac-gliff handoff

State of the port as of 2026-10-10 (late evening), written for the next
session resuming on a real Mac. Everything here is verified unless
marked `[INFERENCE]`. Repo: `teomurgi/mac-gliff` (private).

## What this project is

A macOS port of the **gliff** remote-desktop client (Hyprland over ssh,
4:4:4 H.264 via two AVC444 streams). The Linux client's GPU tier is
VA-API + Vulkan (`gliff-vk`); this port replaces it with **VideoToolbox
decode + Metal compute**, keeping the vendored GTK client and the wire
protocol untouched. The server stays on Linux/Hyprland — only the
client is ported.

Upstream `omacom/gliff @ a2edd50` is vendored byte-identical in commit
`e919614` (minus `.github`/`CLAUDE.md`); everything after that is the
port. `docs/` holds upstream architecture notes that still apply —
read `docs/architecture.md` first if you touch the client.

## Current state (milestones 1 and 2 complete)
## Current state (milestones 1–3 complete)

**Milestone 1** — `crates/gliff-mtl`, the standalone macOS GPU tier:
avcc/display (portable, oracle-tested), `vt` (VideoToolbox session,
hardware default + `new_software()`), `compute` (Metal recombine),
`client` (the client-facing facade: `Gpu::open()` + `Decoder` mirroring
`gliff-vk`'s decoder surface; VM detection via `sysctl
hw.model`). The recombine writes into a fresh **IOSurface** each frame
(`IOSurfaceCreate` + the `newTextureWithDescriptor:iosurface:plane:`
message, raw FFI) and `BgraFrame` carries the surface + row stride;
it is `unsafe impl Send` (the writing command buffer has completed).

**Milestone 2** — the client is wired: `VideoDecoder::Metal` +
`Frame::Metal` in `net.rs` (cfg arms; the Linux arms are textual
copies), per-OS `open_gpu`/`new_decoder` with CPU fallback, the unused
`hypr-ipc`/`drm-fourcc` dropped, the keymap watcher Linux-only.

**Milestone 3** — zero-copy display: `crates/gliff/src/glvideo.rs` (a
`gtk::GLArea` widget) binds each frame's IOSurface with
`CGLTexImageIOSurface2D` and draws it through `paintable::layout`, so
the rectangle, pointer mapping and zoom request match the texture
tiers exactly. `main.rs` routes `Frame::Metal` frames to it (session
`last_frame` caches `Rc<BgraFrame>` for tab switches); Bgra frames
still go through the paintable. Verified on this real Mac by
`cargo run -p gliff --example glvideo`, which decodes the fixture
through the client's own widget and compares the GL framebuffer with
the decoded pixels: **delta 0, both frames**.

Everything green on this machine: `cargo test -p gliff -p gliff-mtl`
(34 + 20 tests + 1 ignored hardware-VT test, which passes),
`cargo clippy -p gliff -p gliff-mtl -p gliff-sw --all-targets --
-D warnings`, `cargo fmt --check`, `gliff --help`, and the
`glvideo` example above. A full client run still needs a Linux box
running `gliff-server --listen`.

Hardware VideoToolbox decode is confirmed on real hardware (the
ignored `hardware_pipeline_matches_the_reference` test passes, ≤1 LSB
vs the reference).


## Set up on a real Mac

```sh
git clone https://github.com/teomurgi/mac-gliff && cd mac-gliff
brew install pkg-config gtk4 libadwaita
mise install rust@latest            # gtk4-rs 0.11.4 needs rustc ≥ 1.92;
mise x rust@latest -- cargo test -p gliff-mtl -p gliff   # the local 1.90 pin is too old
mise x rust@latest -- cargo clippy -p gliff-mtl -p gliff --all-targets -- -D warnings
```

The test fixture is a real x264 stream (High profile, CABAC, VUI) at
`crates/gliff-va/src/h264/testdata/x264-64x64.264`. Gliff's own server
streams differ in one way that matters: **no VUI** (writer asserts
`!vui_present`), High profile, one-in/one-out (no B slices, POC type 0).
Decoding real gliff server output needs a Linux box running
`gliff-server --listen` (dev mode, no auth) — see `docs/development.md`.

## Hard-won facts (do not relearn these)

**Wire/protocol** (from `gliff-proto` + upstream docs):

- One complete access unit per `VideoFrame` payload; SPS/PPS precede
  every IDR, P-frames carry none (session caches them).
- Two independent H.264 streams for AVC444 ("main" + "aux"); the layout
  per 2×2 block is documented in `gliff-proto/src/chroma.rs` and
  mirrored by the shader.
- Client acks each frame immediately (`net.rs` decode loop).

**Apple API facts, all confirmed the hard way**:

- `CMBlockBufferCreateWithMemoryBlock` takes **10** args
  (`offsetToData`, `dataLength` between `customBlockSource` and `flags`)
  and has **no** `sourceBuffer`. Pass `kCFAllocatorNull` as block
  allocator + flags `0` (reference the caller's buffer — FFmpeg's
  pattern).
- `VTDecompressionSessionDecodeFrame` takes **5** args; the trailing
  `VTDecodeInfoFlags *infoFlagsOut` must be NULL or valid — a garbage
  register there was our SIGSEGV inside VT.
- Metal texture writes are **logical RGBA**: `BGRA8Unorm` applies its
  own byte order; never swizzle manually (unlike the GLSL original,
  whose image was a raw rgba8 view).
- `dispatch_threads` (non-uniform) misexecutes on paravirtualized GPUs;
  `dispatch_thread_groups` is what we use.
- Upload textures need `MTLStorageMode::Shared` (default Managed isn't
  GPU-visible without a blit sync; fine on unified memory).
- metal-rs 0.33: `Texture::from_ptr` needs `use foreign_types::ForeignType`;
  `get_bytes(bytes, stride, region, level)`; `set_texture(index, Option<&TextureRef>)`;
  `new_compute_pipeline_state_with_function`. It has **no**
  `max_texture_width/height` getters — zoom is clamped against 16384,
  the macOS Metal minimum. metal types are `!Send` (hence the explicit
  impl on `BgraFrame`).
- `sysctlbyname` needs **no** `#[link]` attribute (rustc links
  libSystem); an explicit `#[link(name = "libSystem", kind = "dylib")]`
  fails to find the library on current SDKs.
- VM detection for the VT hardware/software choice: `sysctl hw.model` —
  real Macs answer `Mac*`/`iMac*` (this machine: `Mac17,6`), Apple VMs
  `VirtualMac*`, others `VMware*`/`Parallels*`/`qemu*`. `kern.hv_support`
  only says the Hypervisor framework exists — useless for this.
- The GitHub macos-15 runner ("Apple Paravirtual device") runs the
  shader tests fine, but **hardware VideoToolbox decode crashes there**;
  tests use software VT. `gliff-mtl::client` auto-detects and the
  `#[ignore]`d hardware test is the real-Mac check.
- Error codes: CMBlockBuffer `-12704` = BadLengthParameter (missing
  offset/dataLength args), `-12705` = BadPointerParameter.
- `CVOpenGLTextureCacheCreateTextureFromImage` takes a **CVImageBuffer**,
  not a bare IOSurface (returns kCVReturnInvalidArgument = -6661). The
  zero-copy GL path for a raw IOSurface is
  `CGLTexImageIOSurface2D(ctx, GL_TEXTURE_RECTANGLE, GL_RGBA8, w, h,
  GL_BGRA, GL_UNSIGNED_INT_8_8_8_8_REV, surface, 0)` — deprecated in
  spirit, works pixel-exact. GTK 4.24's GLArea on macOS gives a real
  desktop CGL context (#version 150 core shaders).
- The GLArea framebuffer is the allocation in device pixels, but the
  widget queries `GL_VIEWPORT` (as GTK left it) instead of guessing.
  With the quad's uv flipped for the IOSurface's top-down rows
  (`uv.y = (2 - pos.y) * 0.5 * tex_size`) the picture lands upright,
  GL bottom row = image bottom row.
- GUI code must run on the process **main thread** (AppKit): a
  `#[test]` that opens a GTK window aborts with "Rust cannot catch
  foreign exceptions" because the test harness thread isn't main.
  That is why the display smoke is an example
  (`cargo run -p gliff --example glvideo`), which `#[path]`-includes
  the client's own `glvideo.rs`/`paintable.rs` (the client is a
  binary crate; examples cannot import from it otherwise).

**Repo/tooling facts**:

- A stale duplicate of the crate used to live at `gliff-mtl/` (repo
  root) next to the real `crates/gliff-mtl` — committed by the very
  first port commit and never cleaned up; it was missing every later
  fix and confused the edit tooling into patching the wrong file.
  Removed in the wiring commit. If files ever seem to "lose" edits,
  check for same-basename duplicates first.
- Workspace `rust-version` is 1.92 (the gtk4-rs 0.11.4 stack's floor).
  The local mise default was 1.90 — too old; use
  `mise x rust@latest -- cargo …`.
- CI (`.github/workflows/ci.yml`) now builds `-p gliff` on both OSes:
  brew GTK on macos, apt `libgtk-4-dev libadwaita-1-dev libva-dev
  libgbm-dev libdrm-dev libxkbcommon-dev libvulkan-dev nasm` on ubuntu.
  The ubuntu list mirrors `pkgbuild/PKGBUILD` depends [INFERENCE: the
  apt closure was not verified on a runner yet; first push may need to
  extend it].
- Workspace lints require SAFETY comments on unsafe blocks — kept up.

## Remaining milestones (README has the checklist)

1. **Input**: kVK→evdev table (main.rs sends `hardware_keycode - 8` as
   evdev codes; GDK on macOS reports `kVK_*`) plus shipping an xkb
   keymap compiled from the active input source (`keymap.rs` currently
   sends none).
2. **Packaging**: Homebrew formula, codesigning.

Also known polish, not blockers: `--video gpu` help text still says
"Vulkan compute + VA-API" (the string lives in `gliff-sw`'s clap
docs); the recombine allocates a fresh IOSurface per frame (gliff-vk
recycles a ring of three); the binary is otherwise unexercised against
a real gliff server on macOS (needs the Linux box).

## Repo conventions

- Rust via mise (`mise x rust@latest -- cargo …` on this machine; CI
  uses dtolnay/rust-toolchain@stable).
- Two-commit history base: vendor commit is pristine; keep port commits
  on top so upstream rebases stay mechanical. Vendored crates may take
  port commits (gliff, gliff-sw already have) — keep them mechanical.
- macOS-only deps go under `[target.'cfg(target_os = "macos")'.dependencies]`
  (see `crates/gliff-mtl/Cargo.toml`, `crates/gliff/Cargo.toml`).
