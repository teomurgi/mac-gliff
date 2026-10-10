//! Metal compute: the AVC444 recombine as a compute pass.
//!
//! The counterpart of `gliff-vk`'s Vulkan pipeline: VideoToolbox decodes
//! into NV12 pixel buffers, `CVMetalTextureCache` wraps their IOSurfaces
//! as `MTLTexture`s without a copy, and `shaders/recombine.metal` writes
//! the BGRA output. The shader is compiled from source at start-up with
//! fast math off, and its output is compared against the CPU reference
//! (`crate::display`) in the tests, which is in turn checked against the
//! `gliff-proto` oracle — the same chain of custody as `gliff-probe
//! pipeline` on Linux.

use std::ffi::c_void;
use std::mem::size_of;

use foreign_types::ForeignType;
use metal::{
    CompileOptions, ComputePipelineState, Device, MTLRegion, MTLSize, MTLStorageMode,
    MTLTextureUsage, Texture, TextureDescriptor, TextureRef,
};

use crate::vt::CVPixelBufferRef;
use crate::{Error, Result};

const SHADER: &str = include_str!("../shaders/recombine.metal");

/// Compute-shader push constants, mirrored in the shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    width: u32,
    height: u32,
    has_aux: u32,
    zoom: u32,
}

/// One recombined frame: the BGRA8 output texture at `zoom`x, plus the
/// CoreVideo texture references that must outlive its use.
pub struct BgraFrame {
    pub texture: Texture,
    /// `CVMetalTextureRef`s backing the sampled planes.
    cv: Vec<*mut c_void>,
    pub width: u32,
    pub height: u32,
    pub zoom: u32,
}

impl BgraFrame {
    /// The pixels, for tests and the CPU fallback. Not the streaming
    /// path: the display side takes the texture.
    pub fn to_bgra(&self) -> Vec<u8> {
        let (w, h) = (
            self.width as usize * self.zoom as usize,
            self.height as usize * self.zoom as usize,
        );
        let mut out = vec![0u8; w * h * 4];
        let stride = (w * 4) as u64;
        // The texture is not in use: the command buffer that wrote it
        // has completed.
        self.texture.get_bytes(
            out.as_mut_ptr() as *mut c_void,
            stride,
            MTLRegion::new_2d(0, 0, w as u64, h as u64),
            0,
        );
        out
    }
}

impl Drop for BgraFrame {
    fn drop(&mut self) {
        // SAFETY: the CVMetalTexture references were created by the
        // texture cache and are released here, after the GPU work.
        for cv in &self.cv {
            // SAFETY: each reference was created by the texture cache
            // and is released exactly once here.
            unsafe { CFRelease(*cv) };
        }
    }
}

// SAFETY: a frame crosses from the decode worker to the UI thread only
// after `dispatch` waited for the command buffer that wrote it, so its
// texture is complete and thereafter read-only; reading pixels
// (`getBytes`) and dropping (releasing the IOSurface-backed
// CVMetalTexture references and the texture) are thread-safe. The frame
// must not be written to on the consuming thread.
unsafe impl Send for BgraFrame {}

/// The recombine pipeline: one device, one queue, one pipeline, one
/// texture cache. Not `Send`; keep it on the decode worker's thread.
pub struct Recombiner {
    device: Device,
    queue: metal::CommandQueue,
    pipeline: ComputePipelineState,
    cache: *mut c_void, // CVMetalTextureCacheRef
}

impl Recombiner {
    pub fn new() -> Result<Self> {
        let device = Device::system_default().ok_or(Error::NoDevice)?;
        let options = CompileOptions::new();
        // Keep the float math identical to the CPU reference.
        options.set_fast_math_enabled(false);
        let library = device
            .new_library_with_source(SHADER, &options)
            .map_err(Error::Metal)?;
        let function = library
            .get_function("recombine", None)
            .map_err(Error::Metal)?;
        let pipeline = device
            .new_compute_pipeline_state_with_function(&function)
            .map_err(Error::Metal)?;
        let queue = device.new_command_queue();
        // SAFETY: the device pointer is live; the created cache is owned.
        let cache = unsafe { cache_create(&device) }?;
        Ok(Self {
            device,
            queue,
            pipeline,
            cache,
        })
    }

    /// The device, for callers that need to interoperate.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Recombine a decoded pair. `main` is required; `aux` carries the
    /// second AVC444 chroma stream. `width`/`height` are the stream size
    /// (the decoded buffers may be larger, aligned by the pool).
    pub fn recombine(
        &self,
        main: CVPixelBufferRef,
        aux: Option<CVPixelBufferRef>,
        width: u32,
        height: u32,
        zoom: u32,
    ) -> Result<BgraFrame> {
        let (w, h) = (width as usize, height as usize);
        let (main_y, main_y_cv) = self.cv_texture(main, w, h, 0)?;
        let (main_uv, main_uv_cv) = self.cv_texture(main, w / 2, h / 2, 1)?;
        let mut cv = vec![main_y_cv, main_uv_cv];
        let (aux_y, aux_uv) = match aux {
            Some(aux) => {
                let (y, y_cv) = self.cv_texture(aux, w, h, 0)?;
                let (uv, uv_cv) = self.cv_texture(aux, w / 2, h / 2, 1)?;
                cv.extend_from_slice(&[y_cv, uv_cv]);
                (y, uv)
            }
            // Without an aux stream the shader never samples slots 2-3;
            // bind the main planes again rather than leaving them unset.
            None => (main_y.clone(), main_uv.clone()),
        };
        let has_aux = aux.is_some();
        let dst = self.output_texture(w * zoom as usize, h * zoom as usize);
        let textures: [&TextureRef; 5] = [&main_y, &main_uv, &aux_y, &aux_uv, &dst];
        self.dispatch(&textures, w, h, has_aux, zoom)?;
        Ok(BgraFrame {
            texture: dst,
            cv,
            width,
            height,
            zoom,
        })
    }

    /// Upload tightly packed NV12 planes and recombine them; the test
    /// path, so the shader can be checked without VideoToolbox.
    pub fn recombine_planes(
        &self,
        main: &gliff_proto::chroma::Nv12,
        aux: Option<&gliff_proto::chroma::Nv12>,
        zoom: u32,
    ) -> Result<BgraFrame> {
        let (w, h) = (main.width, main.height);
        let main_y = self.upload_plane(w, h, 1, &main.y);
        let main_uv = self.upload_plane(w / 2, h / 2, 2, &main.uv);
        let (aux_y, aux_uv) = match aux {
            Some(aux) => (
                self.upload_plane(w, h, 1, &aux.y),
                self.upload_plane(w / 2, h / 2, 2, &aux.uv),
            ),
            None => (main_y.clone(), main_uv.clone()),
        };
        let dst = self.output_texture(w * zoom as usize, h * zoom as usize);
        let textures: [&TextureRef; 5] = [&main_y, &main_uv, &aux_y, &aux_uv, &dst];
        self.dispatch(&textures, w, h, aux.is_some(), zoom)?;
        Ok(BgraFrame {
            texture: dst,
            cv: Vec::new(),
            width: w as u32,
            height: h as u32,
            zoom,
        })
    }

    /// Wrap one plane of an IOSurface-backed pixel buffer as a texture.
    /// Returns the texture and the `CVMetalTextureRef` that must be
    /// released when the frame is done.
    fn cv_texture(
        &self,
        buffer: CVPixelBufferRef,
        w: usize,
        h: usize,
        plane: usize,
    ) -> Result<(Texture, *mut c_void)> {
        let format = if plane == 0 {
            metal::MTLPixelFormat::R8Unorm
        } else {
            metal::MTLPixelFormat::RG8Unorm
        };
        let mut cv: *mut c_void = std::ptr::null_mut();
        // SAFETY: the cache and buffer are live and Metal-compatible; the
        // created reference is owned by the caller.
        let r = unsafe { cache_texture(self.cache, buffer, format as usize, w, h, plane, &mut cv) };
        if r != 0 {
            return Err(Error::Metal(format!(
                "CVMetalTextureCacheCreateTextureFromImage: {r}"
            )));
        }
        // SAFETY: CVMetalTextureGetTexture returns a +0 reference valid
        // while the CVMetalTextureRef is alive; from_ptr takes it over
        // and will over-release without the extra retain, so retain once.
        let texture = unsafe {
            let mtl = CVMetalTextureGetTexture(cv);
            CFRetain(mtl);
            Texture::from_ptr(mtl.cast())
        };
        Ok((texture, cv))
    }

    fn upload_plane(&self, w: usize, h: usize, bytes_per_pixel: usize, bytes: &[u8]) -> Texture {
        let desc = TextureDescriptor::new();
        desc.set_pixel_format(if bytes_per_pixel == 1 {
            metal::MTLPixelFormat::R8Unorm
        } else {
            metal::MTLPixelFormat::RG8Unorm
        });
        desc.set_width(w as u64);
        desc.set_height(h as u64);
        // Shared storage: the CPU upload is immediately GPU-visible.
        // (The default, Managed, is not guaranteed to be without a blit
        // synchronize; Shared needs unified memory, which every machine
        // this port targets has.)
        desc.set_storage_mode(MTLStorageMode::Shared);
        desc.set_usage(MTLTextureUsage::ShaderRead);
        let texture = self.device.new_texture(&desc);
        assert_eq!(bytes.len(), w * h * bytes_per_pixel);
        // `bytes` holds h rows of w*bpp bytes and the stride matches, so
        // the copy stays in bounds.
        texture.replace_region(
            MTLRegion::new_2d(0, 0, w as u64, h as u64),
            0,
            bytes.as_ptr() as *const c_void,
            (w * bytes_per_pixel) as u64,
        );
        texture
    }

    fn output_texture(&self, w: usize, h: usize) -> Texture {
        let desc = TextureDescriptor::new();
        desc.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        desc.set_width(w as u64);
        desc.set_height(h as u64);
        desc.set_usage(MTLTextureUsage::ShaderWrite);
        desc.set_storage_mode(MTLStorageMode::Shared);
        self.device.new_texture(&desc)
    }

    fn dispatch(
        &self,
        textures: &[&TextureRef; 5],
        w: usize,
        h: usize,
        has_aux: bool,
        zoom: u32,
    ) -> Result<()> {
        let params = Params {
            width: w as u32,
            height: h as u32,
            has_aux: has_aux as u32,
            zoom,
        };
        let cmd = self.queue.new_command_buffer();
        let enc = cmd.new_compute_command_encoder();
        enc.set_compute_pipeline_state(&self.pipeline);
        for (i, texture) in textures.iter().enumerate() {
            enc.set_texture(i as u64, Some(texture));
        }
        enc.set_bytes(
            0,
            size_of::<Params>() as u64,
            &params as *const Params as *const c_void,
        );
        let group = MTLSize {
            width: 16,
            height: 16,
            depth: 1,
        };
        let groups = MTLSize {
            width: ((w / 2) as u64).div_ceil(16),
            height: ((h / 2) as u64).div_ceil(16),
            depth: 1,
        };
        // dispatch_thread_groups rather than dispatch_threads: the
        // non-uniform path misexecutes on some virtualized GPUs (the
        // first blocks only), and the grid here is uniform anyway.
        enc.dispatch_thread_groups(groups, group);
        enc.end_encoding();
        cmd.commit();
        cmd.wait_until_completed();
        Ok(())
    }
}

impl Drop for Recombiner {
    fn drop(&mut self) {
        // SAFETY: the cache is owned and no textures from it remain.
        unsafe { CFRelease(self.cache) };
    }
}

// --- CoreVideo / CoreFoundation glue (C APIs, no Objective-C) ----------

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVMetalTextureCacheCreate(
        allocator: *const c_void,
        cache_attributes: *const c_void,
        metal_device: *mut c_void,
        texture_attributes: *const c_void,
        cache_out: *mut *mut c_void,
    ) -> i32;
    fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: *const c_void,
        cache: *mut c_void,
        source_image: *mut c_void,
        texture_attributes: *const c_void,
        pixel_format: usize,
        width: usize,
        height: usize,
        plane_index: usize,
        texture_out: *mut *mut c_void,
    ) -> i32;
    fn CVMetalTextureGetTexture(texture: *mut c_void) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// SAFETY: `device` must be a live MTLDevice for the process lifetime of
/// the cache.
unsafe fn cache_create(device: &Device) -> Result<*mut c_void> {
    let mut cache: *mut c_void = std::ptr::null_mut();
    // SAFETY: the device is live; a null allocator selects the default.
    let r = unsafe {
        CVMetalTextureCacheCreate(
            std::ptr::null(),
            std::ptr::null(),
            device.as_ptr().cast(),
            std::ptr::null(),
            &mut cache,
        )
    };
    if r != 0 {
        return Err(Error::Metal(format!("CVMetalTextureCacheCreate: {r}")));
    }
    Ok(cache)
}

/// SAFETY: `cache` from `cache_create`, `buffer` a Metal-compatible
/// CVImageBuffer of at least `w`x`h` pixels in `plane`.
unsafe fn cache_texture(
    cache: *mut c_void,
    buffer: *mut c_void,
    pixel_format: usize,
    w: usize,
    h: usize,
    plane: usize,
    out: *mut *mut c_void,
) -> i32 {
    // SAFETY: the cache and buffer are live; a null allocator selects the
    // default.
    unsafe {
        CVMetalTextureCacheCreateTextureFromImage(
            std::ptr::null(),
            cache,
            buffer,
            std::ptr::null(),
            pixel_format,
            w,
            h,
            plane,
            out,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display;
    use gliff_proto::chroma::{self, Yuv444};

    fn lcg_u8(seed: &mut u64) -> u8 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as u8
    }

    fn random_yuv444(w: usize, h: usize, mut seed: u64) -> Yuv444 {
        let mut src = Yuv444::new(w, h);
        for plane in [&mut src.y, &mut src.u, &mut src.v] {
            for b in plane.iter_mut() {
                *b = lcg_u8(&mut seed);
            }
        }
        src
    }

    fn max_delta(a: &[u8], b: &[u8]) -> i32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap_or(0)
    }

    fn check(main: &chroma::Nv12, aux: Option<&chroma::Nv12>, zoom: u32) {
        let r = Recombiner::new().expect("a Metal device");
        let got = r
            .recombine_planes(main, aux, zoom)
            .expect("recombine")
            .to_bgra();
        let want = display::to_bgra(main, aux, zoom);
        assert_eq!(got.len(), want.len());
        let delta = max_delta(&got, &want);
        if delta > 1 {
            let at = got
                .iter()
                .zip(&want)
                .position(|(g, w)| (i32::from(*g) - i32::from(*w)).abs() > 1)
                .unwrap_or(0);
            let hex = |b: &[u8]| -> String {
                b.iter()
                    .map(|x| format!("{x:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            eprintln!(
                "device {:?} first-mismatch byte {at} delta {delta}\n  got  {}\n  want {}",
                r.device().name(),
                hex(&got[..32.min(got.len())]),
                hex(&want[..32.min(want.len())]),
            );
        }
        assert!(delta <= 1, "delta {delta}");
    }

    #[test]
    fn shader_matches_the_reference_dual() {
        let src = random_yuv444(64, 64, 11);
        let (main, aux) = chroma::split_yuv444(&src);
        check(&main, Some(&aux), 1);
    }

    #[test]
    fn shader_matches_the_reference_single() {
        let src = random_yuv444(64, 32, 12);
        let main = chroma::yuv444_to_nv12(&src);
        check(&main, None, 1);
    }

    #[test]
    fn shader_matches_the_reference_zoomed() {
        let src = random_yuv444(64, 32, 13);
        let (main, aux) = chroma::split_yuv444(&src);
        check(&main, Some(&aux), 2);
    }

    #[test]
    fn videotoolbox_pipeline_matches_the_reference() {
        use crate::avcc::access_units;
        use crate::vt::Decoder;

        const STREAM: &[u8] = include_bytes!("../../gliff-va/src/h264/testdata/x264-64x64.264");
        let au = &access_units(STREAM)[0];
        let mut d = Decoder::new_software().unwrap();
        let frame = d.decode(au).unwrap().expect("a picture out");
        let nv12 = frame.to_nv12();
        check(&nv12, None, 1);
    }
}
