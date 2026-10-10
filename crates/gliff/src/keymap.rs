//! The local keymap to ship to the server, so every key maps the same on
//! both ends. It follows the active keyboard layout: a change of layout
//! mid-session is sent on.
//!
//! On Linux the compositor's keymap is watched directly (Hyprland via
//! `hypr-input`). On macOS the active keyboard input source is read
//! from Text Input Sources, mapped to the closest xkb layout, and the
//! corresponding pre-generated keymap text (see `scripts/gen-keymaps.sh`)
//! is shipped — the client itself needs no xkb machinery.

use std::time::Duration;

use tokio::sync::watch;

/// The current local keymap (xkb text, format v1); empty until the first
/// one arrives, or for good when the layout cannot be determined.
pub type Keymap = watch::Receiver<String>;

/// How long a new session waits for the first keymap before it lets the
/// server fall back to its default.
pub const FIRST_KEYMAP_WAIT: Duration = Duration::from_secs(1);

pub fn watch() -> Keymap {
    let (tx, rx) = watch::channel(String::new());
    #[cfg(target_os = "linux")]
    {
        let sink = Box::new(move |text: String| {
            tracing::debug!(bytes = text.len(), "local keymap changed");
            tx.send_replace(text);
        });
        if let Err(e) = hypr_input::watch_keymap(hypr_input::Target::default(), sink) {
            tracing::warn!(error = %e, "cannot follow the local keymap; server will default to us");
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::cell::Cell;
        use std::rc::Rc;

        use gtk4::glib;

        use mac::input_source_id;

        // Send the current layout at once: a session waits only briefly
        // for the first keymap before it lets the server default.
        let initial = layout_id(&input_source_id().unwrap_or_default());
        tracing::debug!(layout = initial, "active keyboard layout");
        let seen = Rc::new(Cell::new(initial));
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let id = layout_id(&input_source_id().unwrap_or_default());
            if id != seen.get() {
                tracing::debug!(layout = id, "input source changed");
                seen.set(id);
                tx.send_replace(keymap_text(id).to_owned());
            }
            glib::ControlFlow::Continue
        });
    }
    rx
}

// --- macOS: the active input source, mapped to a shipped keymap ------

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_void};

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        #[link_name = "kTISPropertyInputSourceID"]
        static K_TIS_PROPERTY_INPUT_SOURCE_ID: *const c_void;
        fn TISCopyCurrentKeyboardInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, property: *const c_void) -> *mut c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringGetCString(
            the_string: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> i8;
        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    /// The active keyboard input source's identifier, e.g.
    /// `com.apple.keylayout.German`; `None` when TIS has nothing to say.
    pub fn input_source_id() -> Option<String> {
        // SAFETY: TIS returns a retained source on success; the property
        // value is borrowed from it. Both are handled below.
        unsafe {
            let source = TISCopyCurrentKeyboardInputSource();
            if source.is_null() {
                return None;
            }
            let id = TISGetInputSourceProperty(source, K_TIS_PROPERTY_INPUT_SOURCE_ID);
            let mut buf = [0u8; 128];
            let ok = !id.is_null()
                && CFStringGetCString(
                    id,
                    buf.as_mut_ptr().cast(),
                    buf.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                ) != 0;
            // SAFETY: the source was retained by the copy above and is
            // released exactly once here, after the property read.
            CFRelease(source.cast());
            if !ok {
                return None;
            }
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            Some(String::from_utf8_lossy(&buf[..end]).into_owned())
        }
    }
}

/// The shipped keymap id for a macOS input source identifier: the
/// layout component after the last dot (`com.apple.keylayout.German`
/// → `de`), `us` for anything unrecognized.
#[cfg(target_os = "macos")]
fn layout_id(input_source: &str) -> &'static str {
    let name = input_source.rsplit('.').next().unwrap_or("");
    match name {
        "US" | "ABC" => "us",
        "USInternational-PC" => "us-intl",
        "Dvorak" | "DvorakLeft" | "DvorakRight" => "us-dvorak",
        "Colemak" => "us-colemak",
        "British" | "British-PC" => "gb",
        "French" | "French-PC" => "fr",
        "CanadianFrench" | "CanadianFrench-PC" | "CanadianFrench-ISO" => "ca-fr",
        "German" | "German-PC" => "de",
        "SwissGerman" | "SwissGerman-PC" => "ch-de",
        "SwissFrench" | "SwissFrench-PC" => "ch-fr",
        "Italian" | "Italian-PC" => "it",
        "Spanish" | "Spanish-PC" | "SpanishISO" => "es",
        "LatinAmerican" | "LatinAmerican-PC" => "latam",
        "Dutch" => "nl",
        "Belgian" => "be",
        "Portuguese" | "Portuguese-PC" => "pt",
        "Brazil" | "Brazil-ABNT2" | "Brazil-PC" => "br",
        "Danish" | "Danish-PC" => "dk",
        "Norwegian" | "Norwegian-PC" => "no",
        "Swedish" | "Swedish-PC" => "se",
        "Finnish" | "Finnish-PC" => "fi",
        "Polish" | "PolishPro" | "Polish-PC" => "pl",
        "Czech" => "cz",
        "Hungarian" | "Hungarian-PC" => "hu",
        "Turkish" | "Turkish-Q" | "Turkish-PC" => "tr",
        "Russian" => "ru",
        "Ukrainian" => "ua",
        "Greek" => "gr",
        "Arabic" | "Arabic-PC" | "Arabic-QWERTY" => "ara",
        "Hebrew" | "Hebrew-PC" => "il",
        "Bulgarian" => "bg",
        "Croatian" => "hr",
        "Slovak" => "sk",
        "Slovenian" => "si",
        "Romanian" => "ro",
        "Estonian" => "ee",
        "Lithuanian" => "lt",
        "Latvian" => "lv",
        _ => "us",
    }
}

/// The pre-generated keymap text for a shipped id (format v1, straight
/// from `xkbcli compile-keymap`; the server compiles it as-is).
#[cfg(target_os = "macos")]
fn keymap_text(id: &str) -> &'static str {
    match id {
        "us-intl" => include_str!("../keymaps/us-intl.xkb"),
        "us-dvorak" => include_str!("../keymaps/us-dvorak.xkb"),
        "us-colemak" => include_str!("../keymaps/us-colemak.xkb"),
        "gb" => include_str!("../keymaps/gb.xkb"),
        "fr" => include_str!("../keymaps/fr.xkb"),
        "ca-fr" => include_str!("../keymaps/ca-fr.xkb"),
        "de" => include_str!("../keymaps/de.xkb"),
        "ch-de" => include_str!("../keymaps/ch-de.xkb"),
        "ch-fr" => include_str!("../keymaps/ch-fr.xkb"),
        "it" => include_str!("../keymaps/it.xkb"),
        "es" => include_str!("../keymaps/es.xkb"),
        "latam" => include_str!("../keymaps/latam.xkb"),
        "nl" => include_str!("../keymaps/nl.xkb"),
        "be" => include_str!("../keymaps/be.xkb"),
        "pt" => include_str!("../keymaps/pt.xkb"),
        "br" => include_str!("../keymaps/br.xkb"),
        "dk" => include_str!("../keymaps/dk.xkb"),
        "no" => include_str!("../keymaps/no.xkb"),
        "se" => include_str!("../keymaps/se.xkb"),
        "fi" => include_str!("../keymaps/fi.xkb"),
        "pl" => include_str!("../keymaps/pl.xkb"),
        "cz" => include_str!("../keymaps/cz.xkb"),
        "hu" => include_str!("../keymaps/hu.xkb"),
        "tr" => include_str!("../keymaps/tr.xkb"),
        "ru" => include_str!("../keymaps/ru.xkb"),
        "ua" => include_str!("../keymaps/ua.xkb"),
        "gr" => include_str!("../keymaps/gr.xkb"),
        "ara" => include_str!("../keymaps/ara.xkb"),
        "il" => include_str!("../keymaps/il.xkb"),
        "bg" => include_str!("../keymaps/bg.xkb"),
        "hr" => include_str!("../keymaps/hr.xkb"),
        "sk" => include_str!("../keymaps/sk.xkb"),
        "si" => include_str!("../keymaps/si.xkb"),
        "ro" => include_str!("../keymaps/ro.xkb"),
        "ee" => include_str!("../keymaps/ee.xkb"),
        "lt" => include_str!("../keymaps/lt.xkb"),
        "lv" => include_str!("../keymaps/lv.xkb"),
        _ => include_str!("../keymaps/us.xkb"),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{keymap_text, layout_id};

    /// Input sources map to their xkb layout; unknown ones to `us`.
    #[test]
    fn layouts_map() {
        assert_eq!(layout_id("com.apple.keylayout.German"), "de");
        assert_eq!(layout_id("com.apple.keylayout.US"), "us");
        assert_eq!(layout_id("com.apple.keylayout.ABC"), "us");
        assert_eq!(layout_id("com.apple.keylayout.SwissGerman"), "ch-de");
        assert_eq!(layout_id("com.apple.keylayout.Dvorak"), "us-dvorak");
        assert_eq!(layout_id("com.apple.keylayout.Turkish-Q"), "tr");
        assert_eq!(layout_id("com.apple.inputmethod.SCIM.ITabc"), "us");
        assert_eq!(layout_id(""), "us");
    }

    /// Every id the table can produce has a real keymap behind it, and
    /// the texts are what the server compiles: xkb keymaps starting
    /// with the evdev keycodes block.
    #[test]
    fn shipped_keymaps_exist() {
        let cases = [
            ("German", "de"),
            ("US", "us"),
            ("ABC", "us"),
            ("SwissGerman", "ch-de"),
            ("Dvorak", "us-dvorak"),
            ("Turkish-Q", "tr"),
            ("Ukrainian", "ua"),
            ("Nonsense", "us"),
        ];
        for (source, want) in cases {
            assert_eq!(
                layout_id(&format!("com.apple.keylayout.{source}")),
                want,
                "{source}"
            );
        }
        // Every shipped id is backed by its own keymap: a missing arm
        // in keymap_text falls back to `us`, which every other layout
        // differs from.
        let ids = [
            "us",
            "us-intl",
            "us-dvorak",
            "us-colemak",
            "gb",
            "fr",
            "ca-fr",
            "de",
            "ch-de",
            "ch-fr",
            "it",
            "es",
            "latam",
            "nl",
            "be",
            "pt",
            "br",
            "dk",
            "no",
            "se",
            "fi",
            "pl",
            "cz",
            "hu",
            "tr",
            "ru",
            "ua",
            "gr",
            "ara",
            "il",
            "bg",
            "hr",
            "sk",
            "si",
            "ro",
            "ee",
            "lt",
            "lv",
        ];
        let us = keymap_text("us");
        for id in ids {
            let text = keymap_text(id);
            assert!(text.starts_with("xkb_keymap"), "{id}");
            if id != "us" {
                assert_ne!(text, us, "{id} fell back to the us keymap");
            }
        }
    }
}
