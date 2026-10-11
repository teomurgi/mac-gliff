//! Smoke for the macOS video widget: decode the H.264 fixture through
//! the gliff-mtl pipeline, feed the frames to the client's own
//! `glvideo::GlVideo` (included by path — the client is a binary
//! crate), then read the GLArea's framebuffer back and compare against
//! the frame's own pixels (already checked against the CPU reference by
//! the gliff-mtl tests). Prints one PASS/FAIL per frame and quits.
//!
//! Run on a real Mac with a GUI session:
//! `cargo run -p gliff --example glvideo`
//
// The client is a binary crate, so this example includes its modules
// by path; methods only the full client uses read as dead here. The
// client's own build still lints them.
#![allow(dead_code)]
#[cfg(target_os = "macos")]
#[path = "../src/glvideo.rs"]
mod glvideo;
#[cfg(target_os = "macos")]
#[path = "../src/paintable.rs"]
mod paintable;

#[cfg(target_os = "macos")]
use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::rc::Rc;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use gtk::glib;
#[cfg(target_os = "macos")]
use gtk::prelude::*;
#[cfg(target_os = "macos")]
use gtk4 as gtk;

#[cfg(target_os = "macos")]
use gliff_mtl::avcc::access_units;
#[cfg(target_os = "macos")]
use gliff_mtl::client::{Decoder, Gpu};

#[cfg(target_os = "macos")]
use glvideo::GlVideo;

#[cfg(target_os = "macos")]
const STREAM: &[u8] = include_bytes!("../../gliff-va/src/h264/testdata/x264-64x64.264");

// Readback, only for the verification below.
#[cfg(target_os = "macos")]
#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn glPixelStorei(pname: u32, param: i32);
    fn glGetIntegerv(pname: u32, params: *mut i32);
    fn glReadPixels(
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        data: *mut std::ffi::c_void,
    );
}
#[cfg(target_os = "macos")]
const GL_VIEWPORT: u32 = 0x0BA2;
#[cfg(target_os = "macos")]
const GL_RGBA: u32 = 0x1908;
#[cfg(target_os = "macos")]
const GL_UNSIGNED_BYTE: u32 = 0x1401;
#[cfg(target_os = "macos")]
const GL_PACK_ALIGNMENT: u32 = 0x0D05;

/// Read the framebuffer and compare with the frame's pixels,
/// nearest-upscaled by the display scale the layout used. Returns the
/// worst channel delta.
#[cfg(target_os = "macos")]
fn check(area: &gtk::GLArea, frame: &gliff_mtl::compute::BgraFrame) -> i32 {
    let scale = area.scale_factor().max(1);
    let (w, h) = (
        frame.width as usize * frame.zoom as usize,
        frame.height as usize * frame.zoom as usize,
    );
    // The rect the widget drew, in its own device pixels, mapped into
    // the real framebuffer (whose size GTK decides).
    let Some((dx, dy, dw, dh)) = glvideo::dst_rect(
        (w as u32, h as u32),
        scale,
        (area.width() as f64, area.height() as f64),
    ) else {
        return i32::MAX;
    };
    let mut viewport = [0i32; 4];
    let (fb_w, fb_h);
    // SAFETY: `viewport` is four valid out-integers, queried after
    // make_current on the area's own context.
    unsafe {
        area.make_current();
        glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr());
    }
    (fb_w, fb_h) = (viewport[2].max(1) as f64, viewport[3].max(1) as f64);
    let logical_w = area.width() as f64 * scale as f64;
    let logical_h = area.height() as f64 * scale as f64;
    let map = |v: f64, from: f64, to: f64| (v * to / from).round() as i32;
    let (rx, ry) = (
        map(dx as f64, logical_w, fb_w),
        map(dy as f64, logical_h, fb_h),
    );
    let (rw, rh) = (
        map((dx + dw) as f64, logical_w, fb_w) - rx,
        map((dy + dh) as f64, logical_h, fb_h) - ry,
    );
    let mut read = vec![0u8; rw as usize * rh as usize * 4];
    // SAFETY: the context is current and `read` is rw*rh*4 bytes with
    // PACK_ALIGNMENT=1.
    unsafe {
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        glReadPixels(
            rx,
            ry,
            rw,
            rh,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            read.as_mut_ptr().cast(),
        );
    }
    let reference = frame.to_bgra();
    let (rw, rh) = (rw as usize, rh as usize);
    let mut worst = 0i32;
    for row in 0..rh {
        for col in 0..rw {
            let at = (row * rw + col) * 4;
            let got: &[u8; 4] = read[at..at + 4].try_into().unwrap();
            // Bottom-up framebuffer against the top-down reference,
            // nearest-mapped through the drawn rect.
            let x = col * w / rw;
            let y = h - 1 - row * h / rh;
            let r = (y * w + x) * 4;
            worst = worst
                .max((got[0] as i32 - reference[r + 2] as i32).abs())
                .max((got[1] as i32 - reference[r + 1] as i32).abs())
                .max((got[2] as i32 - reference[r] as i32).abs());
        }
    }
    worst
}

#[cfg(target_os = "macos")]
fn main() {
    let app = gtk::Application::new(None::<&str>, Default::default());
    app.connect_activate(|app| {
        let aus = access_units(STREAM);
        eprintln!("{} access units", aus.len());
        let gpu = Gpu::open().expect("a Metal device");
        let decoder = Decoder::new_software(&gpu, true, 64, 64).expect("the decoder");

        let video = GlVideo::new();
        let window = gtk::ApplicationWindow::new(app);
        window.set_title(Some("gliff GlVideo smoke"));
        let area = video.widget().clone();
        area.set_size_request(64, 64);
        window.set_child(Some(&area));
        let area = area
            .downcast_ref::<gtk::GLArea>()
            .expect("the video widget is the GLArea")
            .clone();
        window.present();

        let state = Rc::new((
            RefCell::new(decoder),
            RefCell::new(None::<Rc<gliff_mtl::compute::BgraFrame>>),
            std::cell::Cell::new(0usize),
            std::cell::Cell::new(0usize),
            std::cell::Cell::new(0usize),
        ));
        glib::timeout_add_local(Duration::from_millis(80), {
            let window = window.clone();
            let state = state.clone();
            let video = video.clone();
            let area = area.clone();
            let aus = aus.clone();
            move || {
                let (decoder, last, frame_no, passes, fails) = &*state;
                // Alternate: check the frame presented last tick (the
                // area rendered in between), then present the next.
                if let Some(frame) = last.borrow_mut().take() {
                    let shown = frame_no.get() - 1;
                    let delta = check(&area, &frame);
                    if delta <= 1 {
                        passes.set(passes.get() + 1);
                        eprintln!("frame {shown} PASS (delta {delta})");
                    } else {
                        fails.set(fails.get() + 1);
                        eprintln!("frame {shown} FAIL (delta {delta})");
                    }
                }
                let i = frame_no.get();
                if i >= aus.len() {
                    eprintln!("smoke done: {} passes, {} fails", passes.get(), fails.get());
                    window.destroy();
                    return glib::ControlFlow::Break;
                }
                let au = &aus[i];
                match decoder.borrow_mut().decode(au, au) {
                    Ok(Some(frame)) => {
                        let frame = Rc::new(frame);
                        *last.borrow_mut() = Some(frame.clone());
                        video.set_frame(frame, area.scale_factor(), (64, 64));
                        frame_no.set(i + 1);
                    }
                    Ok(None) => eprintln!("frame {i}: no picture"),
                    Err(e) => eprintln!("frame {i}: decode error {e}"),
                }
                glib::ControlFlow::Continue
            }
        });
    });
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("glvideo: macOS-only smoke (VideoToolbox + Metal + GLArea)");
}
