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

use crate::vt::{
    CFDictionaryCreate, CFNumberCreate, CVPixelBufferRef, K_CF_TYPE_DICTIONARY_KEY_CALL_BACKS,
    K_CF_TYPE_DICTIONARY_VALUE_CALL_BACKS,
};
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

/// One recombined frame: the BGRA8 output at `zoom`x, written into an
/// IOSurface Metal and GL can share, plus the CoreVideo texture
/// references that must outlive its use.
pub struct BgraFrame {
    pub texture: Texture,
    /// `CVMetalTextureRef`s backing the sampled planes.
    cv: Vec<*mut c_void>,
    /// The IOSurface the output pixels live in, retained.
    surface: *mut c_void,
    /// Bytes per row on the surface; at least `width*4` (row padding).
    stride: usize,
    pub width: u32,
    pub height: u32,
    pub zoom: u32,
}

impl BgraFrame {
    /// The IOSurface holding the pixels, for zero-copy sharing with GL
    /// (`CVOpenGLTextureCache`). The frame keeps it alive; do not
    /// release it.
    pub fn surface(&self) -> *mut c_void {
        self.surface
    }

    /// Bytes per row in [`surface`]'s storage.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// The pixels, tightly packed, for tests and the CPU fallback. Not
    /// the streaming path: the display side takes the IOSurface.
    pub fn to_bgra(&self) -> Vec<u8> {
        let (w, h) = (
            self.width as usize * self.zoom as usize,
            self.height as usize * self.zoom as usize,
        );
        let bytes_per_row = w * 4;
        // The texture is not in use: the command buffer that wrote it
        // has completed.
        if self.stride == bytes_per_row {
            let mut out = vec![0u8; h * bytes_per_row];
            self.texture.get_bytes(
                out.as_mut_ptr() as *mut c_void,
                bytes_per_row as u64,
                MTLRegion::new_2d(0, 0, w as u64, h as u64),
                0,
            );
            return out;
        }
        // A padded surface: read row by row, then compact. `get_bytes`
        // needs the destination rows at the surface's stride.
        let mut padded = vec![0u8; h * self.stride];
        self.texture.get_bytes(
            padded.as_mut_ptr() as *mut c_void,
            self.stride as u64,
            MTLRegion::new_2d(0, 0, w as u64, h as u64),
            0,
        );
        // Copy the image bytes out of each surface row, dropping the
        // padding.
        let mut out = vec![0u8; h * bytes_per_row];
        for row in 0..h {
            let src = &padded[row * self.stride..row * self.stride + bytes_per_row];
            out[row * bytes_per_row..(row + 1) * bytes_per_row].copy_from_slice(src);
        }
        out
    }
}

impl Drop for BgraFrame {
    fn drop(&mut self) {
        // SAFETY: the CVMetalTexture references were created by the
        // texture cache and the IOSurface by `output_texture`; each is
        // released exactly once here, after the GPU work.
        for cv in &self.cv {
            // SAFETY: each reference was created by the texture cache
            // and is released exactly once here.
            unsafe { CFRelease(*cv) };
        }
        // SAFETY: the surface was retained in `output_texture` and is
        // released exactly once here.
        unsafe { CFRelease(self.surface.cast()) };
    }
}

// SAFETY: a frame crosses from the decode worker to the UI thread only
// after `dispatch` waited for the command buffer that wrote it, so its
// texture and IOSurface are complete and thereafter read-only; reading
// pixels (`getBytes`) or handing the IOSurface to GL, and dropping
// (releasing the CF references) are thread-safe. The frame must not be
// written to on the consuming thread.
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
        let (dst, surface, stride) = self.output_texture(w * zoom as usize, h * zoom as usize)?;
        let textures: [&TextureRef; 5] = [&main_y, &main_uv, &aux_y, &aux_uv, &dst];
        self.dispatch(&textures, w, h, has_aux, zoom)?;
        Ok(BgraFrame {
            texture: dst,
            cv,
            surface,
            stride,
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
        let (dst, surface, stride) = self.output_texture(w * zoom as usize, h * zoom as usize)?;
        let textures: [&TextureRef; 5] = [&main_y, &main_uv, &aux_y, &aux_uv, &dst];
        self.dispatch(&textures, w, h, aux.is_some(), zoom)?;
        Ok(BgraFrame {
            texture: dst,
            cv: Vec::new(),
            surface,
            stride,
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

    /// An output texture whose pixels live in a fresh IOSurface, so GL
    /// can sample the same storage zero-copy. The surface is retained
    /// for the frame; the texture releases with it.
    fn output_texture(&self, w: usize, h: usize) -> Result<(Texture, *mut c_void, usize)> {
        let desc = TextureDescriptor::new();
        desc.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        desc.set_width(w as u64);
        desc.set_height(h as u64);
        // ShaderWrite for the recombine; ShaderRead for tests and the
        // CPU fallback, which read the pixels back through the texture.
        desc.set_usage(MTLTextureUsage::ShaderWrite | MTLTextureUsage::ShaderRead);
        // SAFETY: the surface is created for exactly this texture's
        // geometry, and its retention transfers to the frame.
        let (surface, stride) = unsafe { create_surface(w, h)? };
        // SAFETY: the descriptor is freshly configured, the device is
        // live, and the message signature matches the selector
        // (`newTextureWithDescriptor:iosurface:plane:`, plane 0); the
        // result is checked for null.
        let texture = unsafe {
            texture_from_iosurface(self.device.as_ptr().cast(), desc.as_ptr().cast(), surface)
        };
        if texture.is_null() {
            // SAFETY: the surface was retained by `create_surface` and
            // is released here on the failure path.
            unsafe { CFRelease(surface.cast()) };
            return Err(Error::Metal(
                "newTextureWithDescriptor:iosurface: returned nil".into(),
            ));
        }
        // SAFETY: the returned id is an MTLTexture the frame will own.
        let texture = unsafe { Texture::from_ptr(texture.cast()) };
        Ok((texture, surface, stride))
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

// --- IOSurface output allocation, shared with GL -----------------------

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    #[link_name = "kIOSurfaceWidth"]
    static K_IOSURFACE_WIDTH: *const c_void;
    #[link_name = "kIOSurfaceHeight"]
    static K_IOSURFACE_HEIGHT: *const c_void;
    #[link_name = "kIOSurfaceBytesPerRow"]
    static K_IOSURFACE_BYTES_PER_ROW: *const c_void;
    #[link_name = "kIOSurfaceBytesPerElement"]
    static K_IOSURFACE_BYTES_PER_ELEMENT: *const c_void;
    #[link_name = "kIOSurfacePixelFormat"]
    static K_IOSURFACE_PIXEL_FORMAT: *const c_void;
    fn IOSurfaceCreate(properties: *const c_void) -> *mut c_void;
}

// CFDictionaryCreate/CFNumberCreate and the kCFType callback constants
// are declared once, in vt.rs, and shared here.

/// kCFNumberSInt64Type from CFNumber.h.
const K_CF_NUMBER_S_INT_64: isize = 4;

/// 'BGRA' as a fourcc, the IOSurface pixel format matching
/// `MTLPixelFormatBGRA8Unorm` byte for byte.
const BGRA_FOURCC: i64 = 0x4247_5241;

/// A retained `w`x`h` BGRA IOSurface with 64-byte-aligned rows, plus
/// its bytes-per-row (which may exceed `w * 4`).
///
/// # Safety
///
/// The caller must eventually `CFRelease` the returned surface.
unsafe fn create_surface(w: usize, h: usize) -> Result<(*mut c_void, usize)> {
    let stride = (w * 4).div_ceil(64) * 64;
    // SAFETY: the number values are plain integers outliving the call;
    // the empty allocator selects the default.
    let number = |v: i64| unsafe {
        CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_S_INT_64,
            &v as *const i64 as *const c_void,
        )
    };
    let w_n = number(w as i64);
    let h_n = number(h as i64);
    let stride_n = number(stride as i64);
    let elem_n = number(4);
    let format_n = number(BGRA_FOURCC);
    // SAFETY: the kIOSurface* constants are immutable CFString
    // pointers provided by the framework.
    let keys: [*const c_void; 5] = [
        unsafe { K_IOSURFACE_WIDTH },
        unsafe { K_IOSURFACE_HEIGHT },
        unsafe { K_IOSURFACE_BYTES_PER_ROW },
        unsafe { K_IOSURFACE_BYTES_PER_ELEMENT },
        unsafe { K_IOSURFACE_PIXEL_FORMAT },
    ];
    let values: [*const c_void; 5] = [
        w_n.cast(),
        h_n.cast(),
        stride_n.cast(),
        elem_n.cast(),
        format_n.cast(),
    ];
    // SAFETY: keys and values point at `count` valid CF objects, and
    // the type callbacks retain them into the dictionary.
    let props = unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            &K_CF_TYPE_DICTIONARY_KEY_CALL_BACKS,
            &K_CF_TYPE_DICTIONARY_VALUE_CALL_BACKS,
        )
    };
    // SAFETY: `props` holds the numbers; they are released with it.
    for n in values {
        // SAFETY: each number was created above and is released
        // exactly once here.
        unsafe { CFRelease(n) };
    }
    // SAFETY: `props` is a valid property dictionary for a BGRA
    // surface of the stated geometry.
    let surface = unsafe { IOSurfaceCreate(props) };
    // SAFETY: the dictionary was created above and is released here.
    unsafe { CFRelease(props) };
    if surface.is_null() {
        return Err(Error::Metal("IOSurfaceCreate failed".into()));
    }
    Ok((surface, stride))
}

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn sel_registerName(name: *const std::ffi::c_char) -> *const c_void;
    fn objc_msgSend(
        receiver: *mut c_void,
        selector: *const c_void,
        descriptor: *mut c_void,
        surface: *mut c_void,
        plane: usize,
    ) -> *mut c_void;
}

/// `[device newTextureWithDescriptor:iosurface:plane:0]`.
///
/// # Safety
///
/// `device` must be a live MTLDevice, `descriptor` a live
/// MTLTextureDescriptor, and `surface` a BGRA IOSurface matching the
/// descriptor's geometry.
unsafe fn texture_from_iosurface(
    device: *mut c_void,
    descriptor: *mut c_void,
    surface: *mut c_void,
) -> *mut c_void {
    // SAFETY: the argument and return types match the selector's
    // signature exactly (id, SEL, id, IOSurfaceRef, NSUInteger -> id),
    // which is the contract objc_msgSend is called under on arm64.
    unsafe {
        let selector = sel_registerName(c"newTextureWithDescriptor:iosurface:plane:".as_ptr());
        objc_msgSend(device, selector, descriptor, surface, 0)
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
    fn shader_matches_the_reference_padded_stride() {
        // Width 66: 4 bytes per pixel is not a multiple of the
        // surface's 64-byte rows, so readback must compact rows.
        let src = random_yuv444(66, 32, 14);
        let (main, aux) = chroma::split_yuv444(&src);
        let r = Recombiner::new().expect("a Metal device");
        let frame = r.recombine_planes(&main, Some(&aux), 1).expect("recombine");
        assert!(frame.stride() > 66 * 4);
        let want = display::to_bgra(&main, Some(&aux), 1);
        let got = frame.to_bgra();
        assert_eq!(got.len(), want.len());
        let delta = max_delta(&got, &want);
        assert!(delta <= 1, "padded-stride delta {delta}");
    }

    #[test]
    fn videotoolbox_pipeline_matches_the_reference() {
        use crate::avcc::access_units;
        use crate::vt::Decoder;

        const STREAM: &[u8] = include_bytes!("../testdata/x264-64x64.264");
        let au = &access_units(STREAM)[0];
        let mut d = Decoder::new_software().unwrap();
        let frame = d.decode(au).unwrap().expect("a picture out");
        let nv12 = frame.to_nv12();
        check(&nv12, None, 1);
    }
}
