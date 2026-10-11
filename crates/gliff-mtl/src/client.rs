//! The client-facing decode pipeline: both AVC444 streams decoded
//! through VideoToolbox, recombined through Metal into one zoomable
//! BGRA frame per access unit pair — the counterpart of
//! `gliff-vk::pipeline::Decoder`, so the client's decode worker drives
//! both tiers through the same surface.
//!
//! One [`Gpu`] is opened per session and shared by the per-config
//! decoders; one [`Decoder`] per stream configuration, as on Linux.
//! None of it is `Send`: everything stays on the decode worker's
//! thread, like the Linux pipeline's Vulkan objects. The frames handed
//! out are `Send`; see [`compute::BgraFrame`].

use std::ffi::{c_char, c_void};
use std::ptr::null_mut;
use std::sync::Arc;

use crate::compute::{BgraFrame, Recombiner};
use crate::vt::{self, PixelFrame};
use crate::Result;

/// The zoom clamp: every Metal device macOS ships accepts 16384×16384
/// 2D textures, the platform's guaranteed minimum (metal-rs does not
/// expose the per-device limits).
const MAX_TEXTURE_SIDE: u32 = 16_384;

/// The GPU everything decodes on: the Metal device and the recombine
/// pipeline, opened once per session and shared by every stream
/// configuration.
pub struct Gpu {
    /// The device's name, for the connection log.
    pub name: String,
    recombine: Recombiner,
}

impl Gpu {
    /// Open the default Metal device and build the recombine pipeline.
    /// Fails when no usable Metal device exists; the caller falls back
    /// to the CPU tier.
    // The `Arc` shares one Gpu across per-config decoders on the decode
    // worker's single thread; `Recombiner` is !Send by design, so the
    // arc-with-non-send-sync lint does not apply.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn open() -> Result<Arc<Self>> {
        let recombine = Recombiner::new()?;
        Ok(Arc::new(Self {
            name: recombine.device().name().to_owned(),
            recombine,
        }))
    }
}

/// Client side: both streams through VideoToolbox, recombined through
/// Metal. `decode` returns the frame of the access units just fed (the
/// one-in/one-out contract the decode worker builds on), blocks until
/// its pixels are complete, and never buffers a picture across calls.
pub struct Decoder {
    gpu: Arc<Gpu>,
    main: vt::Decoder,
    aux: Option<vt::Decoder>,
    width: u32,
    height: u32,
    zoom: u32,
    /// The last fully decoded pair, kept so a zoom change can redraw a
    /// still screen.
    last: Option<(PixelFrame, Option<PixelFrame>)>,
}

impl Decoder {
    /// Hardware decode by default; a virtual machine gets the software
    /// decoder, because reaching for its absent hardware decoder can
    /// crash VideoToolbox outright.
    pub fn new(gpu: &Arc<Gpu>, dual: bool, width: u32, height: u32) -> Result<Self> {
        Self::with_hardware(gpu, dual, width, height, !virtual_machine())
    }

    /// Force the software decoder, the escape hatch virtual machines
    /// need and the deterministic choice for tests.
    pub fn new_software(gpu: &Arc<Gpu>, dual: bool, width: u32, height: u32) -> Result<Self> {
        Self::with_hardware(gpu, dual, width, height, false)
    }

    fn with_hardware(
        gpu: &Arc<Gpu>,
        dual: bool,
        width: u32,
        height: u32,
        hardware: bool,
    ) -> Result<Self> {
        let open = || {
            if hardware {
                vt::Decoder::new()
            } else {
                vt::Decoder::new_software()
            }
        };
        Ok(Self {
            gpu: gpu.clone(),
            main: open()?,
            aux: if dual { Some(open()?) } else { None },
            width,
            height,
            zoom: 1,
            last: None,
        })
    }

    /// Decode one access unit pair and recombine to a display frame at
    /// the current zoom. `Ok(None)` when the units carried no coded
    /// picture (parameter sets only), or when one stream of a dual pair
    /// has a picture and the other does not — the frame is dropped, as
    /// the Linux pipeline drops it.
    pub fn decode(&mut self, main: &[u8], aux: &[u8]) -> Result<Option<BgraFrame>> {
        let Some(main) = self.main.decode(main)? else {
            return Ok(None);
        };
        let aux = match &mut self.aux {
            Some(decoder) => match decoder.decode(aux)? {
                Some(frame) => Some(frame),
                None => return Ok(None),
            },
            None => None,
        };
        let frame = self.recombine(&main, aux.as_ref())?;
        self.last = Some((main, aux));
        Ok(Some(frame))
    }

    /// Recombine the last decoded pair again at the current zoom, as
    /// after a zoom change on a still screen. `Ok(None)` before the
    /// first complete pair.
    pub fn redraw(&mut self) -> Result<Option<BgraFrame>> {
        match &self.last {
            Some((main, aux)) => Ok(Some(self.recombine(main, aux.as_ref())?)),
            None => Ok(None),
        }
    }

    /// Set the display pixels per stream pixel future recombines
    /// produce, clamped to what a Metal texture may hold, and return
    /// the effective zoom. Cannot fail today; fallible to mirror the
    /// Linux decoder, whose output images it re-allocates.
    pub fn set_zoom(&mut self, zoom: u32) -> Result<u32> {
        let cap = (MAX_TEXTURE_SIDE / self.width.max(1))
            .min(MAX_TEXTURE_SIDE / self.height.max(1))
            .max(1);
        self.zoom = zoom.clamp(1, cap);
        Ok(self.zoom)
    }

    pub fn zoom(&self) -> u32 {
        self.zoom
    }

    fn recombine(&self, main: &PixelFrame, aux: Option<&PixelFrame>) -> Result<BgraFrame> {
        self.gpu.recombine.recombine(
            main.as_buffer(),
            aux.map(|a| a.as_buffer()),
            self.width,
            self.height,
            self.zoom,
        )
    }
}

extern "C" {
    fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> i32;
}

/// Whether this machine reports a virtual machine's `hw.model`: Apple's
/// VMs answer `VirtualMac*`, third-party hypervisors their own names,
/// while every real Mac answers `Mac*` (MacBook, Macmini, MacPro,
/// Mac Studio) or `iMac*`. A failed read assumes real hardware.
fn virtual_machine() -> bool {
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    // SAFETY: `name` is a NUL-terminated constant, `buf` points to
    // `len` writable bytes, and the setter arguments are disabled
    // (null, zero).
    let rc = unsafe {
        sysctlbyname(
            c"hw.model".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            null_mut(),
            0,
        )
    };
    if rc != 0 {
        return false;
    }
    let end = buf[..len].iter().position(|&b| b == 0).unwrap_or(len);
    is_virtual_model(&buf[..end])
}

/// The `hw.model` classifier; separate so the table is testable.
fn is_virtual_model(model: &[u8]) -> bool {
    !(model.starts_with(b"Mac") || model.starts_with(b"iMac"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avcc::{self, access_units};
    use crate::display;
    use gliff_proto::chroma::Nv12;

    const STREAM: &[u8] = include_bytes!("../testdata/x264-64x64.264");

    fn max_delta(a: &[u8], b: &[u8]) -> i32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap_or(0)
    }

    /// The full client pipeline against the CPU reference, which the
    /// portable tests already check against the proto oracle.
    fn check(frame: &BgraFrame, main: &Nv12, aux: Option<&Nv12>, zoom: u32) {
        let got = frame.to_bgra();
        let want = display::to_bgra(main, aux, zoom);
        assert_eq!(got.len(), want.len(), "pixel count");
        let delta = max_delta(&got, &want);
        assert!(delta <= 1, "client pipeline vs reference delta {delta}");
    }

    fn decoded(d: &mut Decoder, main: &[u8], aux: &[u8]) -> BgraFrame {
        d.decode(main, aux).expect("decode").expect("a frame out")
    }

    /// The same unit decoded alone, as the oracle the pipeline must
    /// reproduce: both streams of a dual pair fed the same fixture.
    fn nv12_of(au: &[u8]) -> Nv12 {
        let mut d = vt::Decoder::new_software().unwrap();
        d.decode(au).unwrap().expect("a picture out").to_nv12()
    }

    #[test]
    fn dual_pair_recombines_to_the_reference() {
        let aus = access_units(STREAM);
        let gpu = Gpu::open().unwrap();
        let mut d = Decoder::new_software(&gpu, true, 64, 64).unwrap();
        let nv12 = nv12_of(&aus[0]);
        let frame = decoded(&mut d, &aus[0], &aus[0]);
        assert_eq!((frame.width, frame.height, frame.zoom), (64, 64, 1));
        check(&frame, &nv12, Some(&nv12), 1);
    }

    #[test]
    fn single_stream_recombines_to_the_reference() {
        let aus = access_units(STREAM);
        let gpu = Gpu::open().unwrap();
        let mut d = Decoder::new_software(&gpu, false, 64, 64).unwrap();
        let nv12 = nv12_of(&aus[0]);
        let frame = decoded(&mut d, &aus[0], &[]);
        assert_eq!((frame.width, frame.height, frame.zoom), (64, 64, 1));
        check(&frame, &nv12, None, 1);
    }

    #[test]
    fn zoom_sets_redraw_and_carries() {
        let aus = access_units(STREAM);
        let gpu = Gpu::open().unwrap();
        let mut d = Decoder::new_software(&gpu, true, 64, 64).unwrap();
        // One oracle session: parameter sets arrive with the IDR only.
        let mut oracle = vt::Decoder::new_software().unwrap();
        let first = oracle
            .decode(&aus[0])
            .unwrap()
            .expect("a picture out")
            .to_nv12();
        decoded(&mut d, &aus[0], &aus[0]);
        assert_eq!(d.set_zoom(2).unwrap(), 2);
        let redrawn = d.redraw().unwrap().expect("a redrawn frame");
        assert_eq!((redrawn.width, redrawn.height, redrawn.zoom), (64, 64, 2));
        check(&redrawn, &first, Some(&first), 2);
        // Later decodes pick the zoom up; a clamp keeps it drawable.
        assert_eq!(d.set_zoom(u32::MAX).unwrap(), MAX_TEXTURE_SIDE / 64);
        let next = oracle
            .decode(&aus[1])
            .unwrap()
            .expect("a picture out")
            .to_nv12();
        let frame = decoded(&mut d, &aus[1], &aus[1]);
        assert_eq!(frame.zoom, MAX_TEXTURE_SIDE / 64);
        check(&frame, &next, Some(&next), MAX_TEXTURE_SIDE / 64);
    }

    #[test]
    fn parameter_sets_only_unit_yields_no_frame() {
        let aus = access_units(STREAM);
        // The first unit with its slices stripped: parameter sets only.
        let mut ps_only = Vec::new();
        for nal in avcc::nal_units(&aus[0]) {
            if matches!(nal.nal_type, 7 | 8) {
                ps_only.extend_from_slice(&[0, 0, 1]);
                ps_only.extend_from_slice(nal.data);
            }
        }
        assert!(ps_only.len() < aus[0].len());
        let gpu = Gpu::open().unwrap();
        let mut d = Decoder::new_software(&gpu, false, 64, 64).unwrap();
        assert!(d.decode(&ps_only, &[]).unwrap().is_none());
        // Nothing was retained, so there is nothing to redraw.
        assert!(d.redraw().unwrap().is_none());
    }

    #[test]
    fn virtual_model_classification() {
        for real in ["Mac17,6", "MacBookPro18,3", "Macmini9,1", "iMac24,1"] {
            assert!(!is_virtual_model(real.as_bytes()), "{real}");
        }
        for vm in ["VirtualMac2,1", "VMware7,1", "Parallels14,1", "qemu-arm64"] {
            assert!(is_virtual_model(vm.as_bytes()), "{vm}");
        }
    }

    /// The default constructor reaches for the hardware decoder, which
    /// the CI VM lacks (VideoToolbox crashes there); run explicitly on
    /// a real Mac: `cargo test -p gliff-mtl -- --ignored`.
    #[test]
    #[ignore = "hardware VideoToolbox is absent on the CI VM"]
    fn hardware_pipeline_matches_the_reference() {
        let aus = access_units(STREAM);
        let gpu = Gpu::open().unwrap();
        let mut d = Decoder::new(&gpu, true, 64, 64).unwrap();
        assert!(!virtual_machine(), "this test needs a real Mac");
        let nv12 = nv12_of(&aus[0]);
        let frame = decoded(&mut d, &aus[0], &aus[0]);
        assert_eq!((frame.width, frame.height, frame.zoom), (64, 64, 1));
        check(&frame, &nv12, Some(&nv12), 1);
    }
}
