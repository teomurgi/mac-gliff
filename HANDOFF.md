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

**Milestone 1** — `crates/gliff-mtl`, the standalone macOS GPU tier:

| Module | What | Verified by |
|---|---|---|
| `avcc` | Annex B NAL split, SPS/PPS extraction, AVCC framing, `access_units()` fixture helper | unit tests on both OSes |
| `display` | CPU recombine reference (dual420/single420, zoom) | matches `gliff-proto::color` oracle, ≤1 LSB |
| `vt` | VideoToolbox decoder session, raw C FFI, hardware default + `new_software()` for VMs | decodes the x264 fixture (IDR + P, 64×64 NV12) |
| `compute` | Metal compute recombine, `CVMetalTextureCache` zero-copy input, shader-from-source, fast math off | shader output matches `display` ≤1 LSB, incl. VT→Metal full-pipeline test |
| `client` | the client-facing facade: `Gpu::open()` + `Decoder` mirroring `gliff-vk`'s decoder surface (decode/redraw/set_zoom/zoom), VM detection via `sysctlbyname("hw.model")` picking software VT | 6 tests incl. dual-pair, zoom+redraw, parameter-sets-only; the `#[ignore]`d hardware test passes on a real Mac |

`compute::BgraFrame` is `unsafe impl Send` (documented contract: the
writing command buffer has completed; only read/drop it cross-thread) —
frames cross worker→GTK via the picture channel.

**Milestone 2** — the client (`crates/gliff`) is wired and runs on macOS:

- `net.rs`: `VideoDecoder::Metal` arm (`cfg(target_os = "macos")`),
  per-OS `open_gpu`/`new_decoder` (Metal falls back to the CPU tier),
  `Frame::Metal(gliff_mtl::compute::BgraFrame)` variant, cfg'd decode /
  zoom-redraw / drain arms, `Frame::is_gpu()` +
  `VideoDecoder::returns_just_fed()` replacing the old `matches!`s.
  The Linux arms are textual copies of the originals under cfg.
- `main.rs`: `frame_texture` wraps the Metal frame via `to_bgra()`
  readback into `gdk::MemoryTexture` (one copy; zero-copy display is
  the next milestone); `dmabuf_texture` and the `gliff_vk` import are
  Linux-only.
- `keymap.rs`: the `hypr_input` watcher is Linux-only; macOS ships an
  empty keymap (server defaults) until the input milestone.
- `gliff/Cargo.toml`: `gliff-vk`/`hypr-capture`/`hypr-input` under
  `[target.'cfg(target_os = "linux")'.dependencies]`, `gliff-mtl` under
  macOS; dropped unused `hypr-ipc` and `drm-fourcc`.
- `gliff-sw`: 8 mechanical `chunks_exact` → `as_chunks` conversions —
  clippy 1.99's `chunks_exact_to_as_chunks` fires on `-D warnings`.
  Same for `has_visible_shape` in `main.rs`.
- Verified on this real Mac: `cargo test -p gliff-mtl` (19 + 1
  ignored), `-- --ignored` (hardware VT pipeline, ≤1 LSB vs reference),
  `cargo clippy -p gliff -p gliff-sw -p gliff-mtl --all-targets
  -- -D warnings`, `cargo fmt`, and `gliff --help` from a debug build.

**Hardware VideoToolbox decode is confirmed on real hardware** — the
open question from the previous handoff is closed.

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

1. **Display zero-copy**: `gtk::GLArea` + `CVOpenGLTextureCache`
   sharing the IOSurface Metal writes. Needs IOSurface-backed output
   textures in `compute` (`device.new_texture_with_descriptor` +
   IOSurface, not the current plain textures), then `main.rs`'s
   `frame_texture` Metal arm swaps readback for the GLArea path.
   Risk item; spike first.
2. **Input**: kVK→evdev table (main.rs sends `hardware_keycode - 8` as
   evdev codes; GDK on macOS reports `kVK_*`) plus shipping an xkb
   keymap compiled from the active input source (`keymap.rs` currently
   sends none).
3. **Packaging**: Homebrew formula, codesigning.

Also known polish, not blockers: `--video gpu` help text still says
"Vulkan compute + VA-API" (the string lives in `gliff-sw`'s clap docs);
Metal recombine allocates a fresh output texture per frame (gliff-vk
recycles a ring of three); `gliff --help` wording aside, the binary is
otherwise unexercised against a real server on macOS.

## Repo conventions

- Rust via mise (`mise x rust@latest -- cargo …` on this machine; CI
  uses dtolnay/rust-toolchain@stable).
- Two-commit history base: vendor commit is pristine; keep port commits
  on top so upstream rebases stay mechanical. Vendored crates may take
  port commits (gliff, gliff-sw already have) — keep them mechanical.
- macOS-only deps go under `[target.'cfg(target_os = "macos")'.dependencies]`
  (see `crates/gliff-mtl/Cargo.toml`, `crates/gliff/Cargo.toml`).
