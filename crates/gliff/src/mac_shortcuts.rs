//! macOS system hotkey capture for the foreground stream.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

// SDL's Cocoa keyboard grab uses the same WindowServer connection API:
// https://github.com/libsdl-org/SDL/blob/7e8576fd1967b16e38b862bba7cddb92e54ea695/src/video/cocoa/SDL_cocoakeyboard.m
const HOTKEYS_ENABLED: c_int = 0;
const HOTKEYS_DISABLED: c_int = 1;
const RTLD_DEFAULT: *mut c_void = -2_isize as *mut c_void;

type DefaultConnection = unsafe extern "C" fn() -> c_int;
type SetHotkeyMode = unsafe extern "C" fn(c_int, c_int) -> c_int;

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

struct HotkeyApi {
    connection: DefaultConnection,
    set_mode: SetHotkeyMode,
}

impl HotkeyApi {
    fn resolve() -> Option<Self> {
        // The GTK window already uses AppKit, which loads the system libraries
        // providing these symbols. A lookup keeps them optional at launch.
        // SAFETY: RTLD_DEFAULT is Darwin's global lookup handle and both names
        // are NUL-terminated C strings.
        let (connection, set_mode) = unsafe {
            (
                dlsym(RTLD_DEFAULT, c"_CGSDefaultConnection".as_ptr()),
                dlsym(RTLD_DEFAULT, c"CGSSetGlobalHotKeyOperatingMode".as_ptr()),
            )
        };
        if connection.is_null() || set_mode.is_null() {
            return None;
        }
        // SAFETY: These function pointers match the C signatures SDL uses.
        // The system libraries stay loaded for the lifetime of the GUI.
        Some(unsafe {
            Self {
                connection: std::mem::transmute::<*mut c_void, DefaultConnection>(connection),
                set_mode: std::mem::transmute::<*mut c_void, SetHotkeyMode>(set_mode),
            }
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the macOS system shortcut API is unavailable")]
    Unavailable,
    #[error("macOS rejected the system shortcut mode (CGError {0})")]
    Rejected(c_int),
}

/// Change the hotkey mode for this app's WindowServer connection.
/// Call from the GTK main thread once the window has been realized.
pub fn set_captured(captured: bool) -> Result<(), Error> {
    static API: OnceLock<Option<HotkeyApi>> = OnceLock::new();

    let api = API
        .get_or_init(HotkeyApi::resolve)
        .as_ref()
        .ok_or(Error::Unavailable)?;
    let mode = if captured {
        HOTKEYS_DISABLED
    } else {
        HOTKEYS_ENABLED
    };
    // SAFETY: The setter receives the connection returned by the platform and
    // one of the two operating modes described above.
    let status = unsafe { (api.set_mode)((api.connection)(), mode) };
    if status != 0 {
        return Err(Error::Rejected(status));
    }
    Ok(())
}
