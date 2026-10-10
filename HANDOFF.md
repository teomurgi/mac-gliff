# mac-gliff handoff

State of the port as of 2026-10-10, written for a fresh session resuming
on a real Mac. Everything here is verified unless marked `[INFERENCE]`.
Repo: `teomurgi/mac-gliff` (private). CI green:
ubuntu (portable tests + `gliff-proto`) and macos-15 (full tier).

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

## Current state (milestone 1 complete)

`crates/gliff-mtl` — the macOS GPU tier, standalone, not yet wired into
the client:

| Module | What | Verified by |
|---|---|---|
| `avcc` | Annex B NAL split (from `gliff-va`), SPS/PPS extraction, AVCC framing, `access_units()` fixture helper | unit tests on both OSes |
| `display` | CPU recombine reference (dual420/single420, zoom) | matches `gliff-proto::color` oracle, ≤1 LSB |
| `vt` | VideoToolbox decoder session, raw C FFI, hardware default + `new_software()` for VMs | decodes the x264 fixture on the macOS runner (IDR + P, 64×64 NV12) |
| `compute` | Metal compute recombine, `CVMetalTextureCache` zero-copy input, shader-from-source with fast math off | shader output matches `display` ≤1 LSB on the runner, incl. VT→Metal full-pipeline test |
| `shaders/recombine.metal` | MSL transliteration of `gliff-vk`'s `recombine.comp` | same oracle chain |

The pixel contract's chain of custody: `gliff-proto` (oracle) →
`display.rs` (reference) → MSL shader — mirroring `gliff-probe
pipeline` on Linux.

## Set up on a real Mac

```sh
git clone https://github.com/teomurgi/mac-gliff && cd mac-gliff
cargo test -p gliff-mtl          # 14 tests; VT + Metal run for real
cargo clippy -p gliff-mtl --all-targets -- -D warnings
```

No system deps beyond Xcode CLT (`xcode-select --install`); GTK is only
needed once the client wiring milestone starts (`brew install gtk4
libadwaita pkg-config`).

The test fixture is a real x264 stream (High profile, CABAC, VUI) at
`crates/gliff-va/src/h264/testdata/x264-64x64.264`. Gliff's own server
streams differ in one way that matters: **no VUI** (writer asserts
`!vui_present`), High profile, one-in/one-out (no B slices, POC type 0).
Decoding real gliff server output needs a Linux box running
`gliff-server --listen` (dev mode, no auth) — see
`docs/development.md` upstream.

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
  guaranteed GPU-visible without a blit sync; fine on unified memory).
- The GitHub macos-15 runner ("Apple Paravirtual device") runs the
  shader tests fine, but **hardware VideoToolbox decode is absent
  there**; tests use `Decoder::new_software()`. On a real Mac the
  default (`Decoder::new()`) takes the hardware path — worth confirming
  `videotoolbox_pipeline_matches_the_reference` still passes there.
- metal-rs 0.33: `Texture::from_ptr` needs `use foreign_types::ForeignType`;
  `get_bytes(bytes, stride, region, level)`; `set_texture(index, Option<&TextureRef>)`;
  `new_compute_pipeline_state_with_function`.
- Error codes: CMBlockBuffer `-12704` = BadLengthParameter (missing
  offset/dataLength args), `-12705` = BadPointerParameter (our extra
  null landing in `blockBufferOut`).

**Client seams for the wiring milestone** (all in vendored `crates/gliff`):

- `src/net.rs:188` `enum VideoDecoder { Gpu, Cpu }` — add a `Metal` arm
  here; `open_gpu` (net.rs:196) is the graceful-fallback pattern to copy.
- `src/net.rs:112` `enum Frame { Dmabuf, Bgra }` — add a Surface/Texture
  variant; `Frame::Dmabuf` geometry tracking (`last_gpu_geometry`) shows
  the redraw/zoom contract.
- `src/main.rs:1074` `frame_texture` / `dmabuf_texture` — the display
  handoff; this is where the GLArea+`CVOpenGLTextureCache` IOSurface
  path slots in (GTK4 has no IOSurface API; that plan is in README).
- `src/main.rs:1536-1570` — keyboard sends `hardware_keycode - 8` as
  evdev codes. On macOS GDK reports `kVK_*` codes: needs a positional
  kVK→evdev table. `src/keymap.rs` ships an xkb keymap from the local
  Wayland seat — on macOS, compile one with libxkbcommon
  (`xkb_keymap_new_from_names`) from the active input source instead.
- `Decoder::decode` contract in `gliff-mtl::vt` already matches what
  `net.rs` expects of the GPU decoder: one picture out per submitted
  AU, synchronous wait inside.

## Remaining milestones (README has the checklist)

1. **Wire in**: `VideoDecoder::Metal` arm behind
   `cfg(target_os = "macos")`; cfg-gate `gliff-vk`/`gliff-va`/
   `hypr-capture`/`hypr-input` deps out of the `gliff` crate on darwin
   (they link libgbm/libva — the macOS build fails at link otherwise).
   The `gliff` crate's `hypr-ipc` dep is likely dead weight (unused in
   client sources).
2. **Display**: GLArea + `CVOpenGLTextureCache` sharing the IOSurface
   Metal writes (zero-copy). Risk item; spike it first.
3. **Input**: kVK→evdev table + keymap shipping.
4. **Packaging**: Homebrew formula, codesigning.

CI: `.github/workflows/ci.yml` (matrix ubuntu/macos-15, fmt+clippy+test
with `-D warnings`). The workspace lints require SAFETY comments on
unsafe blocks — keep that up.

## Repo conventions

- Rust via rustup on the Mac (CI uses dtolnay/rust-toolchain@stable).
- Two-commit history base: vendor commit is pristine; keep port commits
  on top so upstream rebases stay mechanical.
- macOS-only deps go under `[target.'cfg(target_os = "macos")'.dependencies]`
  in `crates/gliff-mtl/Cargo.toml` (metal, foreign-types, parking_lot).
