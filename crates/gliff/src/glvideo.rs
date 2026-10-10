//! The macOS video widget: a `gtk::GLArea` that draws the decoder's
//! output IOSurface zero-copy. Metal writes the recombined frame into
//! the surface; `CGLTexImageIOSurface2D` binds that same storage as a
//! GL rectangle texture inside the area's render, so no pixel is copied
//! between decode and display.
//!
//! The rectangle math is [`crate::paintable::layout`], the same source
//! the texture path uses, so the pointer mapping, the zoom request and
//! the on-screen placement agree between the tiers.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::paintable;
use gliff_mtl::compute::BgraFrame;

// --- OpenGL / CGL, raw -------------------------------------------------

#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn CGLGetCurrentContext() -> *mut c_void;
    fn CGLTexImageIOSurface2D(
        ctx: *mut c_void,
        target: u32,
        internal_format: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        iosurface: *mut c_void,
        plane: u32,
    ) -> i32;
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: u32);
    fn glViewport(x: i32, y: i32, w: i32, h: i32);
    fn glGetIntegerv(pname: u32, params: *mut i32);
    fn glCreateShader(kind: u32) -> u32;
    fn glShaderSource(shader: u32, count: i32, srcs: *const *const c_char, lengths: *const i32);
    fn glCompileShader(shader: u32);
    fn glGetShaderiv(shader: u32, pname: u32, params: *mut i32);
    fn glGetShaderInfoLog(shader: u32, max: i32, len: *mut i32, info_log: *mut c_char);
    fn glGetProgramiv(prog: u32, pname: u32, params: *mut i32);
    fn glCreateProgram() -> u32;
    fn glAttachShader(prog: u32, shader: u32);
    fn glLinkProgram(prog: u32);
    fn glUseProgram(prog: u32);
    fn glGetUniformLocation(prog: u32, name: *const c_char) -> i32;
    fn glUniform4f(location: i32, x: f32, y: f32, z: f32, w: f32);
    fn glUniform2f(location: i32, x: f32, y: f32);
    fn glGenVertexArrays(n: i32, arrays: *mut u32);
    fn glBindVertexArray(array: u32);
    fn glGenTextures(n: i32, textures: *mut u32);
    fn glTexParameteri(target: u32, pname: u32, param: i32);
    fn glBindTexture(target: u32, texture: u32);
    fn glDrawArrays(mode: u32, first: i32, count: i32);
}

const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_TEXTURE_RECTANGLE: u32 = 0x84F5;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_NEAREST: i32 = 0x2600;
const GL_LINEAR: i32 = 0x2601;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_TRUE: i32 = 1;
const GL_RGBA8: i32 = 0x8058;
const GL_BGRA: u32 = 0x80E1;
const GL_UNSIGNED_INT_8_8_8_8_REV: u32 = 0x8367;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_VIEWPORT: u32 = 0x0BA2;

/// The quad's destination in normalized device coordinates: x0,y0 is
/// the bottom-left corner, x1,y1 the top-right. IOSurface rows are
/// top-down while GL's are bottom-up, so the quad's bottom edge maps
/// to the image's last row and the picture lands upright.
const VERTEX_SRC: &CStr = c"
#version 150
out vec2 uv;
uniform vec4 dst;
uniform vec2 tex_size;
void main() {
    vec2 pos = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(mix(dst.xy, dst.zw, pos * 0.5), 0.0, 1.0);
    uv = vec2(pos.x, 2.0 - pos.y) * 0.5 * tex_size;
}
";

const FRAGMENT_SRC: &CStr = c"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2DRect tex;
void main() { color = texture(tex, uv); }
";

fn compile(kind: u32, src: &CStr) -> u32 {
    // SAFETY: standard GL object creation; the source pointer is a
    // NUL-terminated C string with an explicit length.
    let shader = unsafe { glCreateShader(kind) };
    let ptr = src.as_ptr();
    let len = src.to_bytes().len() as i32;
    // SAFETY: the shader id is fresh and the source pointer/length
    // describe one NUL-terminated string.
    unsafe {
        glShaderSource(shader, 1, &ptr, &len);
        glCompileShader(shader);
    }
    let mut ok = 0;
    // SAFETY: `ok` is a valid out-parameter.
    unsafe { glGetShaderiv(shader, GL_COMPILE_STATUS, &mut ok) };
    if ok != GL_TRUE {
        let mut log = [0 as c_char; 1024];
        let mut written = 0;
        // SAFETY: `log` is a valid buffer of the stated size.
        unsafe { glGetShaderInfoLog(shader, log.len() as i32, &mut written, log.as_mut_ptr()) };
        let text = log[..written.max(0) as usize]
            .iter()
            .map(|&c| c as u8)
            .collect::<Vec<_>>();
        panic!("shader compile: {}", String::from_utf8_lossy(&text));
    }
    shader
}

/// Where the frame lands in the framebuffer, in device pixels, with GL's
/// bottom-left origin: `(x, y, w, h)` with `y` counted from the bottom.
/// Mirrors [`paintable::layout`] for a widget of `alloc` logical pixels
/// (width, height) on a display of `scale` device pixels per logical one.
pub(crate) fn dst_rect(
    view: (u32, u32),
    scale: i32,
    alloc: (f64, f64),
) -> Option<(i32, i32, i32, i32)> {
    let scale = scale.max(1);
    let l = paintable::layout(view, scale, alloc.0, alloc.1)?;
    let fb_h = alloc.1 * scale as f64;
    let (x, y) = (l.x * scale as f64, l.y * scale as f64);
    let (w, h) = (l.width * scale as f64, l.height * scale as f64);
    // The layout counts from the top; GL from the bottom. The texture
    // is drawn scaled into this rect whatever its own size, exactly as
    // the paintable scales its texture node into the same rect.
    Some((
        x.round() as i32,
        (fb_h - y - h).round() as i32,
        w.round() as i32,
        h.round() as i32,
    ))
}

/// Owns one GLArea and everything needed to draw frames into it. Not
/// `Send`: GTK objects, and the GL state belongs to the area's context.
pub struct GlVideo {
    area: gtk::GLArea,
    ctx: Cell<*mut c_void>,
    program: Cell<u32>,
    texture: Cell<u32>,
    frame: RefCell<Option<Rc<BgraFrame>>>,
    view: Cell<(u32, u32)>,
    scale: Cell<i32>,
}

impl GlVideo {
    pub fn new() -> Rc<Self> {
        let area = gtk::GLArea::new();
        let video = Rc::new(Self {
            area,
            ctx: Cell::new(std::ptr::null_mut()),
            program: Cell::new(0),
            texture: Cell::new(0),
            frame: RefCell::new(None),
            view: Cell::new((0, 0)),
            scale: Cell::new(1),
        });
        video.area.connect_render(glib::clone!(
            #[strong]
            video,
            move |_, _| {
                video.render();
                glib::Propagation::Proceed
            }
        ));
        video
    }

    /// The widget to pack.
    pub fn widget(&self) -> &gtk::Widget {
        self.area.upcast_ref()
    }

    /// Show `frame` inside a `view`-sized area (device pixels) on a
    /// display of `scale_factor`.
    pub fn set_frame(&self, frame: Rc<BgraFrame>, scale_factor: i32, view: (u32, u32)) {
        *self.frame.borrow_mut() = Some(frame);
        self.scale.set(scale_factor);
        self.view.set(view);
        self.area.queue_render();
    }

    /// Follow a display scale change without waiting for a new frame.
    pub fn set_scale(&self, scale_factor: i32) {
        self.scale.set(scale_factor);
        self.area.queue_render();
    }

    /// Show nothing, as when no machine is connected.
    pub fn clear(&self) {
        if self.frame.borrow_mut().take().is_some() {
            self.view.set((0, 0));
            self.area.queue_render();
        }
    }

    fn ensure_gl(&self) {
        if self.program.get() != 0 {
            return;
        }
        // SAFETY: all ids are fresh out-parameters.
        unsafe {
            let mut vao = 0;
            glGenVertexArrays(1, &mut vao);
            glBindVertexArray(vao);
            let mut texture = 0;
            glGenTextures(1, &mut texture);
            self.texture.set(texture);
        }
        let vs = compile(GL_VERTEX_SHADER, VERTEX_SRC);
        let fs = compile(GL_FRAGMENT_SHADER, FRAGMENT_SRC);
        // SAFETY: standard GL program setup on fresh shader ids.
        let program = unsafe { glCreateProgram() };
        // SAFETY: attach and link on the fresh program and shader ids.
        unsafe {
            glAttachShader(program, vs);
            glAttachShader(program, fs);
            glLinkProgram(program);
        }
        let mut ok = 0;
        // SAFETY: `ok` is a valid out-parameter.
        unsafe { glGetProgramiv(program, GL_LINK_STATUS, &mut ok) };
        assert_eq!(ok, GL_TRUE, "GL program link failed");
        self.program.set(program);
        // SAFETY: called inside render, so the area's context is
        // current; CGLGetCurrentContext returns exactly it.
        let ctx = unsafe { CGLGetCurrentContext() };
        assert!(!ctx.is_null(), "no current CGL context in GLArea render");
        self.ctx.set(ctx);
    }

    fn render(&self) {
        self.ensure_gl();
        let scale = self.scale.get().max(1);
        let (alloc_w, alloc_h) = (self.area.width() as f64, self.area.height() as f64);
        // The framebuffer's real size, as GTK left the viewport: it is
        // the allocation in device pixels, but asking beats guessing.
        let mut viewport = [0i32; 4];
        // SAFETY: `viewport` is four valid out-integers.
        unsafe { glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr()) };
        let (fb_w, fb_h) = (viewport[2], viewport[3]);
        // SAFETY: valid program and a fresh clear over the whole
        // framebuffer of the current context.
        unsafe {
            glUseProgram(self.program.get());
            glViewport(0, 0, fb_w, fb_h);
            glClearColor(0.0, 0.0, 0.0, 1.0);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        let Some(frame) = self.frame.borrow().clone() else {
            return;
        };
        let Some((x, y, w, h)) = dst_rect(self.view.get(), scale, (alloc_w, alloc_h)) else {
            return;
        };
        let (tex_w, tex_h) = (
            frame.width as i32 * frame.zoom as i32,
            frame.height as i32 * frame.zoom as i32,
        );
        // The paintable path keeps an integer factor pixel-exact with
        // nearest sampling and shrinks with linear; mirror it.
        let filter = if w >= tex_w { GL_NEAREST } else { GL_LINEAR };
        // SAFETY: the texture object exists and is bound to the
        // rectangle target; the IOSurface belongs to the live frame and
        // holds tex_w*tex_h BGRA pixels; the context is current.
        unsafe {
            glBindTexture(GL_TEXTURE_RECTANGLE, self.texture.get());
            glTexParameteri(GL_TEXTURE_RECTANGLE, GL_TEXTURE_MIN_FILTER, filter);
            glTexParameteri(GL_TEXTURE_RECTANGLE, GL_TEXTURE_MAG_FILTER, filter);
            let err = CGLTexImageIOSurface2D(
                self.ctx.get(),
                GL_TEXTURE_RECTANGLE,
                GL_RGBA8,
                tex_w,
                tex_h,
                GL_BGRA,
                GL_UNSIGNED_INT_8_8_8_8_REV,
                frame.surface(),
                0,
            );
            assert_eq!(err, 0, "CGLTexImageIOSurface2D: {err}");
            let (fb_w, fb_h) = (fb_w as f32, fb_h as f32);
            let to_ndc = |px: f32, size: f32| px / size * 2.0 - 1.0;
            let dst = glGetUniformLocation(self.program.get(), c"dst".as_ptr());
            glUniform4f(
                dst,
                to_ndc(x as f32, fb_w),
                to_ndc(y as f32, fb_h),
                to_ndc((x + w) as f32, fb_w),
                to_ndc((y + h) as f32, fb_h),
            );
            let tex_size = glGetUniformLocation(self.program.get(), c"tex_size".as_ptr());
            glUniform2f(tex_size, tex_w as f32, tex_h as f32);
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dst_rect;

    /// An exact fit: the view fills the widget and the frame is 1:1.
    #[test]
    fn full_fit_is_one_to_one() {
        let r = dst_rect((64, 64), 2, (32.0, 32.0)).unwrap();
        assert_eq!(r, (0, 0, 64, 64));
    }

    /// Letterboxing centres a smaller view, offsets snapped to device
    /// pixels; the rect keeps GL's bottom-left origin.
    #[test]
    fn letterbox_centres() {
        let r = dst_rect((64, 64), 1, (96.0, 64.0)).unwrap();
        assert_eq!(r, (16, 0, 64, 64));
        let r = dst_rect((64, 64), 1, (64.0, 96.0)).unwrap();
        assert_eq!(r, (0, 16, 64, 64));
    }

    /// The rect is the layout's: whatever size the frame's texture has
    /// (a stale zoom, say), it is scaled into the same rectangle the
    /// paintable would use.
    #[test]
    fn the_rect_follows_the_layout_not_the_texture() {
        let r = dst_rect((32, 32), 1, (32.0, 32.0)).unwrap();
        assert_eq!(r, (0, 0, 32, 32));
        let r = dst_rect((32, 32), 2, (64.0, 64.0)).unwrap();
        assert_eq!(r, (0, 0, 128, 128));
    }
}
