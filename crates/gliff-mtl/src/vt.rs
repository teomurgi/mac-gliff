//! VideoToolbox H.264 decode for the gliff client's macOS tier.
//!
//! One `Decoder` per elementary stream: gliff sends the main and the
//! auxiliary AVC444 stream as two independent H.264 streams. Each
//! protocol `VideoFrame` payload is one complete access unit; parameter
//! sets are pulled out for the format description and the slices are
//! framed as AVCC ([`crate::avcc`]). The decode call waits for the output
//! callback, so the caller sees one picture out per submitted frame —
//! the same contract the VA-API decoder gives the Linux client (no B
//! slices, POC type 0).
//!
//! The FFI is raw `extern "C"` against CoreFoundation, CoreMedia,
//! CoreVideo, VideoToolbox and Dispatch; no Objective-C is involved.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::avcc;
use crate::{Error, Result};

/// A decoded picture. Owns a `CVPixelBufferRef` (NV12, two planes,
/// IOSurface-backed and Metal-compatible).
#[derive(Debug)]
pub struct PixelFrame {
    buffer: CVPixelBufferRef,
}

impl PixelFrame {
    pub fn width(&self) -> usize {
        // SAFETY: buffer is a live CVPixelBufferRef owned by self.
        unsafe { CVPixelBufferGetWidth(self.buffer) }
    }

    pub fn height(&self) -> usize {
        // SAFETY: buffer is a live CVPixelBufferRef owned by self.
        unsafe { CVPixelBufferGetHeight(self.buffer) }
    }

    /// The buffer itself, for the Metal texture cache.
    pub fn as_buffer(&self) -> CVPixelBufferRef {
        self.buffer
    }

    /// A tightly packed copy of the NV12 planes (tests and the CPU
    /// reference; the streaming path never maps the pixels).
    pub fn to_nv12(&self) -> gliff_proto::chroma::Nv12 {
        let (w, h) = (self.width(), self.height());
        let mut out = gliff_proto::chroma::Nv12::new(w, h);
        // SAFETY: lock/unlock bracket the CPU mapping of a live pixel
        // buffer; the plane addresses are valid while locked.
        unsafe {
            assert_eq!(CVPixelBufferLockBaseAddress(self.buffer, 0), 0);
            for plane in 0..2 {
                let base = CVPixelBufferGetBaseAddressOfPlane(self.buffer, plane) as *const u8;
                let stride = CVPixelBufferGetBytesPerRowOfPlane(self.buffer, plane);
                let rows = if plane == 0 { h } else { h / 2 };
                let dst = if plane == 0 { &mut out.y } else { &mut out.uv };
                for row in 0..rows {
                    std::ptr::copy_nonoverlapping(
                        base.add(row * stride),
                        dst[row * w..(row + 1) * w].as_mut_ptr(),
                        w,
                    );
                }
            }
            CVPixelBufferUnlockBaseAddress(self.buffer, 0);
        }
        out
    }
}

impl Drop for PixelFrame {
    fn drop(&mut self) {
        // SAFETY: the buffer was retained for self and is live until here.
        unsafe { CVPixelBufferRelease(self.buffer) };
    }
}

/// Output callback state, shared with VideoToolbox's decode thread.
struct Shared {
    sem: Semaphore,
    pending: Mutex<Option<Owned>>,
}

/// A pixel buffer handed from the decode callback thread to the owner.
/// CF objects are reference-counted and safe to pass between threads;
/// no CPU mapping is live while it sits in the slot.
#[derive(Clone, Copy)]
struct Owned(CVPixelBufferRef);

// SAFETY: see the struct docs.
unsafe impl Send for Owned {}

/// The parameter sets a session was built from, owned.
#[derive(Default, PartialEq, Eq, Debug, Clone)]
struct OwnedPs {
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

impl OwnedPs {
    fn from(ps: &avcc::ParameterSets<'_>) -> Self {
        Self {
            sps: ps.sps.iter().map(|s| s.to_vec()).collect(),
            pps: ps.pps.iter().map(|p| p.to_vec()).collect(),
        }
    }
}

/// A decoding session for one elementary stream. Not `Send`: keep it on
/// the thread that feeds it, as the Linux pipeline keeps its GPU objects.
pub struct Decoder {
    session: VTDecompressionSessionRef,
    format: CMFormatDescriptionRef,
    ps: OwnedPs,
    shared: Arc<Shared>,
    /// Presentation timestamp counter, in 600ths of a second.
    frame: AtomicI64,
    /// Whether hardware decode is allowed. Forced off in virtualized
    /// machines, where reaching for the (absent) hardware decoder can
    /// crash VideoToolbox outright.
    hardware: bool,
}

impl Decoder {
    /// The session is built from the stream's own SPS/PPS at the first
    /// access unit; hardware decode when the machine has it.
    pub fn new() -> Result<Self> {
        Self::with_hardware(true)
    }

    /// Force the software decoder; for virtual machines whose
    /// VideoToolbox crashes when asked for hardware decode.
    pub fn new_software() -> Result<Self> {
        Self::with_hardware(false)
    }

    fn with_hardware(hardware: bool) -> Result<Self> {
        // SAFETY: creating a semaphore with count 0 is always valid.
        let sem = unsafe { dispatch_semaphore_create(0) };
        Ok(Self {
            session: null_mut(),
            format: null_mut(),
            ps: OwnedPs::default(),
            shared: Arc::new(Shared {
                sem: Semaphore(sem),
                pending: Mutex::new(None),
            }),
            frame: AtomicI64::new(0),
            hardware,
        })
    }

    /// Decode one complete access unit. `Ok(None)` when the unit carries
    /// no coded slice (parameter sets only).
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<PixelFrame>> {
        if let Some(ps) = avcc::parameter_sets(au) {
            let owned = OwnedPs::from(&ps);
            if owned != self.ps {
                self.rebuild(&owned)?;
                self.ps = owned;
            }
        }
        if self.session.is_null() {
            return Err(Error::Stream(
                "no parameter sets seen; the first access unit must carry SPS and PPS".into(),
            ));
        }
        let sample = avcc::avcc(au);
        if sample.is_empty() {
            return Ok(None);
        }
        *self.shared.pending.lock() = None;

        // SAFETY: the block buffer references the caller's sample, which
        // outlives the synchronous decode; all handles are checked.
        unsafe {
            let mut block: CMBlockBufferRef = null_mut();
            check(
                "CMBlockBufferCreateWithMemoryBlock",
                CMBlockBufferCreateWithMemoryBlock(
                    K_CF_ALLOCATOR_DEFAULT,
                    sample.as_ptr() as *const c_void,
                    sample.len(),
                    K_CF_ALLOCATOR_NULL,
                    null(),
                    0,
                    sample.len(),
                    0,
                    &mut block,
                ),
            )?;
            let pts = CMTime {
                value: self.frame.fetch_add(1, Ordering::SeqCst) + 1,
                timescale: 600,
                flags: CM_TIME_FLAGS_VALID,
                epoch: 0,
            };
            let timing = CMSampleTimingInfo {
                duration: CMTime::INVALID,
                presentationTimeStamp: pts,
                decodeTimeStamp: CMTime::INVALID,
            };
            let mut sb: CMSampleBufferRef = null_mut();
            check(
                "CMSampleBufferCreateReady",
                CMSampleBufferCreateReady(
                    K_CF_ALLOCATOR_DEFAULT,
                    block,
                    self.format,
                    1,
                    1,
                    &timing,
                    0,
                    null(),
                    &mut sb,
                ),
            )?;
            check(
                "VTDecompressionSessionDecodeFrame",
                VTDecompressionSessionDecodeFrame(self.session, sb, 0, null_mut(), null_mut()),
            )?;
            CFRelease(sb.cast());
            CFRelease(block.cast());
        }

        // SAFETY: the semaphore was created with count 0 and is signalled
        // exactly once per decode callback.
        unsafe {
            dispatch_semaphore_wait(self.shared.sem.0, DISPATCH_TIME_FOREVER);
        }
        let buffer = self.shared.pending.lock().take().map(|b| b.0);
        Ok(buffer.map(|buffer| PixelFrame { buffer }))
    }

    /// Build the format description and the session for new parameter
    /// sets, replacing any previous ones.
    fn rebuild(&mut self, ps: &OwnedPs) -> Result<()> {
        let format = create_format_description(ps)?;
        let attrs = destination_attributes()?;
        let spec = decoder_specification(self.hardware)?;
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_output),
            decompressionOutputRefCon: Arc::as_ptr(&self.shared) as *mut c_void,
        };
        let mut session: VTDecompressionSessionRef = null_mut();
        // SAFETY: all arguments are live for the call; the record's
        // callback context is the shared state, which outlives the
        // session (dropped after invalidate).
        unsafe {
            check(
                "VTDecompressionSessionCreate",
                VTDecompressionSessionCreate(
                    K_CF_ALLOCATOR_DEFAULT,
                    format,
                    spec,
                    attrs,
                    &record,
                    &mut session,
                ),
            )?;
            CFRelease(spec.cast());
            CFRelease(attrs.cast());
            self.teardown_session();
        }
        self.session = session;
        if !self.format.is_null() {
            // SAFETY: format was created with a +1 reference.
            unsafe { CFRelease(self.format.cast()) };
        }
        self.format = format;
        Ok(())
    }

    /// Invalidate and release the current session, if any.
    ///
    /// # Safety
    /// The session must not be used afterwards until replaced.
    unsafe fn teardown_session(&mut self) {
        if !self.session.is_null() {
            // SAFETY: the session is owned, idle (waited) and invalidated
            // here, and not used again unless replaced by a new create.
            unsafe {
                VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session.cast());
            }
            self.session = null_mut();
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the decoder owns the session and the format
        // description, and nothing else uses them after this.
        unsafe {
            self.teardown_session();
            if !self.format.is_null() {
                CFRelease(self.format.cast());
            }
            dispatch_semaphore_signal(self.shared.sem.0);
            dispatch_release(self.shared.sem.0.cast());
        }
    }
}

/// The decode output callback: retains the picture and signals.
unsafe extern "C" fn on_output(
    ctx: *mut c_void,
    _source_frame: *mut c_void,
    status: i32,
    _flags: u32,
    image: CVImageBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: ctx is the Arc<Shared> pointer handed to session creation;
    // the callback runs while the session (and so the decoder) is alive.
    let shared = unsafe { &*(ctx as *const Shared) };
    if status == 0 && !image.is_null() {
        // SAFETY: the image buffer is live for the callback; the retain
        // transfers ownership to the pending slot.
        unsafe { CVPixelBufferRetain(image) };
        *shared.pending.lock() = Some(Owned(image));
    } else if status != 0 {
        tracing::warn!(code = status, "videotoolbox decode status");
    }
    // SAFETY: signalling a live semaphore.
    unsafe { dispatch_semaphore_signal(shared.sem.0) };
}

fn check(call: &'static str, code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Status { call, code })
    }
}

/// The decoder specification: whether hardware decode may be used.
fn decoder_specification(hardware: bool) -> Result<CFDictionaryRef> {
    // SAFETY: the key and values are live CF globals; the dictionary
    // owns nothing beyond them.
    unsafe {
        let keys = [K_VT_ENABLE_HARDWARE_DECODE];
        let values = [if hardware {
            K_CF_BOOLEAN_TRUE
        } else {
            K_CF_BOOLEAN_FALSE
        }];
        Ok(CFDictionaryCreate(
            K_CF_ALLOCATOR_DEFAULT,
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &K_CF_TYPE_DICTIONARY_KEY_CALL_BACKS,
            &K_CF_TYPE_DICTIONARY_VALUE_CALL_BACKS,
        ))
    }
}

/// The pixel format and Metal compatibility VideoToolbox's output pool
/// needs for `CVMetalTextureCache`.
fn destination_attributes() -> Result<CFDictionaryRef> {
    // SAFETY: the CF objects used as keys and values are live globals or
    // freshly created; the dictionary owns the number afterwards.
    unsafe {
        let format: i32 = CV_PIXEL_FORMAT_420V as i32;
        let number = CFNumberCreate(
            K_CF_ALLOCATOR_DEFAULT,
            CF_NUMBER_SINT32,
            &format as *const i32 as *const c_void,
        );
        let keys = [
            K_CVPIXELBUFFER_PIXEL_FORMAT_TYPE_KEY,
            K_CVPIXELBUFFER_METAL_COMPATIBILITY_KEY,
        ];
        let values = [number, K_CF_BOOLEAN_TRUE];
        let dict = CFDictionaryCreate(
            K_CF_ALLOCATOR_DEFAULT,
            keys.as_ptr(),
            values.as_ptr(),
            2,
            &K_CF_TYPE_DICTIONARY_KEY_CALL_BACKS,
            &K_CF_TYPE_DICTIONARY_VALUE_CALL_BACKS,
        );
        CFRelease(number.cast());
        Ok(dict)
    }
}

fn create_format_description(ps: &OwnedPs) -> Result<CMFormatDescriptionRef> {
    let mut pointers: Vec<*const u8> = Vec::with_capacity(ps.sps.len() + ps.pps.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(pointers.capacity());
    for nal in ps.sps.iter().chain(ps.pps.iter()) {
        pointers.push(nal.as_ptr());
        sizes.push(nal.len());
    }
    let mut format: CMFormatDescriptionRef = null_mut();
    // SAFETY: the parameter set pointers and sizes describe the owned
    // vectors, which are live for the call; the created description is
    // owned by the caller.
    unsafe {
        check(
            "CMVideoFormatDescriptionCreateFromH264ParameterSets",
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                K_CF_ALLOCATOR_DEFAULT,
                pointers.len(),
                pointers.as_ptr(),
                sizes.as_ptr(),
                4, // AVCC length-prefix size, matching crate::avcc
                &mut format,
            ),
        )?;
    }
    Ok(format)
}

// ---------------------------------------------------------------------------
// Raw FFI. Types follow the C headers; pointers to framework objects are
// opaque. Every `unsafe` use is at a call site above with its own SAFETY
// argument.
// ---------------------------------------------------------------------------
/// An owned dispatch semaphore; safe to share across threads by design.
#[derive(Clone, Copy)]
struct Semaphore(DispatchSemaphore);

// SAFETY: a dispatch semaphore is thread-safe; the pointer is opaque and
// only passed to dispatch functions.
unsafe impl Send for Semaphore {}
// SAFETY: see above.
unsafe impl Sync for Semaphore {}

pub type CVPixelBufferRef = *mut c_void;
type CVImageBufferRef = *mut c_void;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFNumberRef = *const c_void;
type CFStringRef = *const c_void;
type CMFormatDescriptionRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type VTDecompressionSessionRef = *mut c_void;
type DispatchSemaphore = *mut c_void;

/// `kCMTimeFlags_Valid`.
const CM_TIME_FLAGS_VALID: u32 = 0x1;

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` ('420v').
const CV_PIXEL_FORMAT_420V: u32 = 0x3432_3076;

/// `kCFNumberSInt32Type`.
const CF_NUMBER_SINT32: isize = 3;

/// `DISPATCH_TIME_FOREVER`.
const DISPATCH_TIME_FOREVER: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    /// An invalid timestamp (all zero flags).
    const INVALID: CMTime = CMTime {
        value: 0,
        timescale: 0,
        flags: 0,
        epoch: 0,
    };
}

#[repr(C)]
#[allow(non_snake_case)] // field names follow the C headers
struct CMSampleTimingInfo {
    duration: CMTime,
    presentationTimeStamp: CMTime,
    decodeTimeStamp: CMTime,
}

/// `CFDictionaryKeyCallBacks`: version plus five callback pointers.
#[repr(C)]
pub(crate) struct CFDictionaryKeyCallBacks {
    version: isize,
    retain: usize,
    release: usize,
    copy_description: usize,
    equal: usize,
    hash: usize,
}

/// `CFDictionaryValueCallBacks`: version plus four callback pointers.
#[repr(C)]
pub(crate) struct CFDictionaryValueCallBacks {
    version: isize,
    retain: usize,
    release: usize,
    copy_description: usize,
    equal: usize,
}

#[repr(C)]
#[allow(non_snake_case)] // field names follow the C headers
struct VTDecompressionOutputCallbackRecord {
    decompressionOutputCallback: Option<
        unsafe extern "C" fn(*mut c_void, *mut c_void, i32, u32, CVImageBufferRef, CMTime, CMTime),
    >,
    decompressionOutputRefCon: *mut c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    #[link_name = "kCFAllocatorDefault"]
    static K_CF_ALLOCATOR_DEFAULT: CFAllocatorRef;
    #[link_name = "kCFAllocatorNull"]
    static K_CF_ALLOCATOR_NULL: CFAllocatorRef;
    #[link_name = "kCFBooleanTrue"]
    static K_CF_BOOLEAN_TRUE: *const c_void;
    #[link_name = "kCFBooleanFalse"]
    static K_CF_BOOLEAN_FALSE: *const c_void;
    #[link_name = "kCFTypeDictionaryKeyCallBacks"]
    pub(crate) static K_CF_TYPE_DICTIONARY_KEY_CALL_BACKS: CFDictionaryKeyCallBacks;
    #[link_name = "kCFTypeDictionaryValueCallBacks"]
    pub(crate) static K_CF_TYPE_DICTIONARY_VALUE_CALL_BACKS: CFDictionaryValueCallBacks;
    fn CFRelease(cf: *const c_void);
    pub(crate) fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
    pub(crate) fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: isize,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    #[link_name = "kCVPixelBufferPixelFormatTypeKey"]
    static K_CVPIXELBUFFER_PIXEL_FORMAT_TYPE_KEY: CFStringRef;
    #[link_name = "kCVPixelBufferMetalCompatibilityKey"]
    static K_CVPIXELBUFFER_METAL_COMPATIBILITY_KEY: CFStringRef;
    fn CVPixelBufferRetain(buffer: CVPixelBufferRef) -> CVPixelBufferRef;
    fn CVPixelBufferRelease(buffer: CVPixelBufferRef);
    fn CVPixelBufferGetWidth(buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(
        buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CVPixelBufferRef, plane_index: usize) -> usize;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMFormatDescriptionRef,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: CFAllocatorRef,
        memory_block: *const c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> i32;
    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        format_description: CMFormatDescriptionRef,
        num_samples: isize,
        num_sample_timing_entries: isize,
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: isize,
        sample_size_array: *const usize,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> i32;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    #[link_name = "kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder"]
    static K_VT_ENABLE_HARDWARE_DECODE: CFStringRef;
    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: CMFormatDescriptionRef,
        video_source_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        session_out: *mut VTDecompressionSessionRef,
    ) -> i32;
    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample_buffer: CMSampleBufferRef,
        decode_flags: u32,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> i32;
    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: VTDecompressionSessionRef) -> i32;
}

// The dispatch semaphore functions live in libSystem, which is linked
// by default; there is no standalone Dispatch framework to link.
extern "C" {
    fn dispatch_semaphore_create(value: isize) -> DispatchSemaphore;
    fn dispatch_semaphore_wait(sema: DispatchSemaphore, timeout: u64) -> isize;
    fn dispatch_semaphore_signal(sema: DispatchSemaphore) -> isize;
    fn dispatch_release(object: *const c_void);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avcc::access_units;

    /// x264 High profile, 64x64, two frames (IDR + P).
    const STREAM: &[u8] = include_bytes!("../../gliff-va/src/h264/testdata/x264-64x64.264");

    #[test]
    fn decodes_the_fixture() {
        let aus = access_units(STREAM);
        assert_eq!(aus.len(), 2, "IDR + P");
        // The runner's GPU is paravirtualized: hardware decode crashes
        // VideoToolbox there, so the fixture decodes in software.
        let mut d = Decoder::new_software().unwrap();
        let idr = d.decode(&aus[0]).unwrap().expect("a picture out");
        assert_eq!((idr.width(), idr.height()), (64, 64));
        let nv12 = idr.to_nv12();
        assert_eq!(nv12.y.len(), 64 * 64);
        assert_eq!(nv12.uv.len(), 64 * 64 / 2);
        let p = d.decode(&aus[1]).unwrap().expect("a picture out");
        assert_eq!((p.width(), p.height()), (64, 64));
    }

    #[test]
    fn rejects_units_without_parameter_sets() {
        let mut d = Decoder::new_software().unwrap();
        let err = d.decode(&[0, 0, 1, 0x41, 0x9a, 0x02, 0x05]).unwrap_err();
        assert!(matches!(err, Error::Stream(_)), "{err}");
    }
}
