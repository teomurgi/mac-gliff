//! The macOS GPU tier for a gliff client: VideoToolbox H.264 decode and a
//! Metal compute recombine, the counterpart of `gliff-vk`'s VA-API +
//! Vulkan pipeline on Linux.
//!
//! Two halves:
//!
//! - Portable (`avcc`, `display`): the Annex B -> AVCC framing
//!   VideoToolbox needs, and a CPU reference of the client display
//!   recombine. Both are unit-tested against the `gliff-proto` oracle on
//!   every platform, so the port's pixel contract is checked wherever the
//!   suite runs.
//! - macOS-only (`vt`, `compute`): the VideoToolbox decoder session and
//!   the Metal compute pipeline. CI builds and exercises these on macOS;
//!   the shader's output is compared against `display` there.
//!
//! The wire stream is Annex B elementary H.264 with SPS and PPS in front
//! of every IDR, and each protocol `VideoFrame` payload is one complete
//! access unit, so no access-unit assembly is needed. The AVC444 split
//! carries no B slices and POC type 0: one picture leaves per submitted
//! frame, the same one-in/one-out contract the VA-API decoder gives
//! `gliff`'s decode worker.

pub mod avcc;
pub mod display;

#[cfg(target_os = "macos")]
pub mod client;
#[cfg(target_os = "macos")]
pub mod compute;
#[cfg(target_os = "macos")]
pub mod vt;

/// Errors of the macOS decode path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("videotoolbox: {call} returned {code}")]
    Status { call: &'static str, code: i32 },
    #[error("metal: {0}")]
    Metal(String),
    #[error("no usable GPU")]
    NoDevice,
    #[error("stream: {0}")]
    Stream(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
