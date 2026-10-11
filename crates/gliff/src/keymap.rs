//! The local keymap to ship to the server, so every key maps the same on
//! both ends. It follows the active keyboard layout: a change of layout
//! mid-session is sent on.
//!
//! Upstream's Linux client watches the compositor's keymap directly
//! (Hyprland via `hypr-input`). On macOS the active keyboard input source is read
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
        tx.send_replace(keymap_text(initial).to_owned());
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
        "AfghanDari" => "af",
        "AfghanPashto" => "af-ps",
        "AfghanUzbek" => "af-uz",
        "Albanian" => "al",
        "Armenian-HMQWERTY" | "Armenian-WesternQWERTY" => "am",
        "Arabic" | "Arabic-PC" | "Arabic-QWERTY" | "ArabicPC" => "ara",
        "Arabic-AZERTY" | "Arabic-NorthAfrica" => "ara-azerty",
        "Austrian" => "at",
        "Australian" => "au",
        "Azeri" => "az",
        "Belgian" => "be",
        "Bulgarian" => "bg",
        "Bulgarian-Phonetic" => "bg-phonetic",
        "Brazil" | "Brazil-ABNT2" | "Brazil-PC" | "Brazilian" | "Brazilian-ABNT2"
        | "Brazilian-Pro" => "br",
        "Dzongkha" => "bt",
        "Byelorussian" => "by",
        "Canadian-CSA" | "CanadianFrench" | "CanadianFrench-ISO" | "CanadianFrench-PC" => "ca-fr",
        "SwissGerman" | "SwissGerman-PC" => "ch-de",
        "SwissFrench" | "SwissFrench-PC" => "ch-fr",
        "Uyghur" => "cn-ug",
        "Czech" => "cz",
        "Czech-QWERTY" => "cz-qwerty",
        "ABC-QWERTZ" | "German" | "German-PC" => "de",
        "Danish" | "Danish-PC" => "dk",
        "Kabyle-AZERTY" => "dz",
        "Estonian" => "ee",
        "Spanish" | "Spanish-ISO" | "Spanish-PC" | "SpanishISO" => "es",
        "Geez-QWERTY" => "et",
        "Finnish" | "Finnish-PC" | "FinnishExtended" => "fi",
        "Faroese" => "fo",
        "ABC-AZERTY" | "French" | "French-PC" | "French-numerical" => "fr",
        "British" | "British-PC" | "Welsh" => "gb",
        "Georgian-QWERTY" => "ge",
        "Greek" => "gr",
        "GreekPolytonic" => "gr-polytonic",
        "Croatian" | "Croatian-PC" => "hr",
        "Hungarian" | "Hungarian-PC" => "hu",
        "Hungarian-QWERTY" => "hu-qwerty",
        "Irish" | "IrishExtended" => "ie",
        "Hebrew" | "Hebrew-PC" => "il",
        "Kurdish-Sorani" => "iq-ku",
        "Persian" | "Persian-ISIRI2901" => "ir",
        "Icelandic" => "is",
        "Italian" | "Italian-PC" | "Italian-Pro" => "it",
        "KANA" => "jp-kana",
        "Kyrgyz-Cyrillic" => "kg",
        "Khmer" => "kh",
        "2SetHangul" | "390Hangul" | "3SetHangul" => "kr",
        "Kazakh" => "kz",
        "Lao" => "la",
        "LatinAmerican" | "LatinAmerican-PC" => "latam",
        "Lithuanian" | "Lithuanian-LST1582" => "lt",
        "Latvian" => "lv",
        "Tifinagh-AZERTY" => "ma-tifinagh",
        "Macedonian" => "mk",
        "Myanmar" => "mm",
        "Mongolian-Cyrillic" => "mn",
        "Maltese" => "mt",
        "Hausa" | "Igbo" | "Yoruba" => "ng",
        "Dutch" => "nl",
        "Norwegian" | "Norwegian-PC" | "NorwegianExtended" => "no",
        "FinnishSami-PC" | "InariSami" | "JulevSami" | "JulevSami-Norway" | "KildinSami"
        | "NorthernSami" | "NorwegianSami-PC" | "PiteSami" | "Sami-PC" | "SkoltSami"
        | "SouthernSami" | "SwedishSami-PC" | "UmeSami" => "no-smi",
        "Nepali" | "Nepali-IS16350" => "np",
        "NewZealand" => "nz",
        "Urdu" => "pk",
        "Polish" | "Polish-PC" | "PolishPro" => "pl",
        "Portuguese" | "Portuguese-PC" => "pt",
        "Romanian" | "Romanian-Standard" => "ro",
        "Serbian" => "rs",
        "Serbian-Latin" => "rs-latin",
        "Russian" | "RussianWin" => "ru",
        "Chuvash" => "ru-cv",
        "Russian-Phonetic" => "ru-phonetic",
        "Yakut" => "ru-sah",
        "Swedish" | "Swedish-PC" | "Swedish-Pro" => "se",
        "Slovenian" | "Slovenian-QWERTZ" => "si",
        "Slovak" => "sk",
        "Slovak-QWERTY" => "sk-qwerty",
        "Thai" => "th",
        "Tajik-Cyrillic" => "tj",
        "Turkmen" => "tm",
        "Turkish" | "Turkish-PC" | "Turkish-Q" | "Turkish-QWERTY" | "Turkish-QWERTY-PC" => "tr",
        "Turkish-Standard" => "tr-f",
        "Kurdish-Kurmanji" => "tr-ku",
        "Ukrainian" | "Ukrainian-PC" | "Ukrainian-QWERTY" => "ua",
        "ABC" | "US" | "USExtended" => "us",
        "Colemak" => "us-colemak",
        "DVORAK-QWERTYCMD" | "Dvorak" | "Dvorak-Left" | "Dvorak-Right" => "us-dvorak",
        "USInternational-PC" => "us-intl",
        "Uzbek-Cyrillic" => "uz",
        "Vietnamese" => "vn",
        _ => "us",
    }
}

/// The pre-generated keymap text for a shipped id (format v1, straight
/// from `xkbcli compile-keymap`; the server compiles it as-is).
#[cfg(target_os = "macos")]
fn keymap_text(id: &str) -> &'static str {
    match id {
        "af" => include_str!("../keymaps/af.xkb"),
        "af-ps" => include_str!("../keymaps/af-ps.xkb"),
        "af-uz" => include_str!("../keymaps/af-uz.xkb"),
        "al" => include_str!("../keymaps/al.xkb"),
        "am" => include_str!("../keymaps/am.xkb"),
        "ara" => include_str!("../keymaps/ara.xkb"),
        "ara-azerty" => include_str!("../keymaps/ara-azerty.xkb"),
        "at" => include_str!("../keymaps/at.xkb"),
        "au" => include_str!("../keymaps/au.xkb"),
        "az" => include_str!("../keymaps/az.xkb"),
        "be" => include_str!("../keymaps/be.xkb"),
        "bg" => include_str!("../keymaps/bg.xkb"),
        "bg-phonetic" => include_str!("../keymaps/bg-phonetic.xkb"),
        "br" => include_str!("../keymaps/br.xkb"),
        "bt" => include_str!("../keymaps/bt.xkb"),
        "by" => include_str!("../keymaps/by.xkb"),
        "ca-fr" => include_str!("../keymaps/ca-fr.xkb"),
        "ch-de" => include_str!("../keymaps/ch-de.xkb"),
        "ch-fr" => include_str!("../keymaps/ch-fr.xkb"),
        "cn-ug" => include_str!("../keymaps/cn-ug.xkb"),
        "cz" => include_str!("../keymaps/cz.xkb"),
        "cz-qwerty" => include_str!("../keymaps/cz-qwerty.xkb"),
        "de" => include_str!("../keymaps/de.xkb"),
        "dk" => include_str!("../keymaps/dk.xkb"),
        "dz" => include_str!("../keymaps/dz.xkb"),
        "ee" => include_str!("../keymaps/ee.xkb"),
        "es" => include_str!("../keymaps/es.xkb"),
        "et" => include_str!("../keymaps/et.xkb"),
        "fi" => include_str!("../keymaps/fi.xkb"),
        "fo" => include_str!("../keymaps/fo.xkb"),
        "fr" => include_str!("../keymaps/fr.xkb"),
        "gb" => include_str!("../keymaps/gb.xkb"),
        "ge" => include_str!("../keymaps/ge.xkb"),
        "gr" => include_str!("../keymaps/gr.xkb"),
        "gr-polytonic" => include_str!("../keymaps/gr-polytonic.xkb"),
        "hr" => include_str!("../keymaps/hr.xkb"),
        "hu" => include_str!("../keymaps/hu.xkb"),
        "hu-qwerty" => include_str!("../keymaps/hu-qwerty.xkb"),
        "ie" => include_str!("../keymaps/ie.xkb"),
        "il" => include_str!("../keymaps/il.xkb"),
        "iq-ku" => include_str!("../keymaps/iq-ku.xkb"),
        "ir" => include_str!("../keymaps/ir.xkb"),
        "is" => include_str!("../keymaps/is.xkb"),
        "it" => include_str!("../keymaps/it.xkb"),
        "jp-kana" => include_str!("../keymaps/jp-kana.xkb"),
        "kg" => include_str!("../keymaps/kg.xkb"),
        "kh" => include_str!("../keymaps/kh.xkb"),
        "kr" => include_str!("../keymaps/kr.xkb"),
        "kz" => include_str!("../keymaps/kz.xkb"),
        "la" => include_str!("../keymaps/la.xkb"),
        "latam" => include_str!("../keymaps/latam.xkb"),
        "lt" => include_str!("../keymaps/lt.xkb"),
        "lv" => include_str!("../keymaps/lv.xkb"),
        "ma-tifinagh" => include_str!("../keymaps/ma-tifinagh.xkb"),
        "mk" => include_str!("../keymaps/mk.xkb"),
        "mm" => include_str!("../keymaps/mm.xkb"),
        "mn" => include_str!("../keymaps/mn.xkb"),
        "mt" => include_str!("../keymaps/mt.xkb"),
        "ng" => include_str!("../keymaps/ng.xkb"),
        "nl" => include_str!("../keymaps/nl.xkb"),
        "no" => include_str!("../keymaps/no.xkb"),
        "no-smi" => include_str!("../keymaps/no-smi.xkb"),
        "np" => include_str!("../keymaps/np.xkb"),
        "nz" => include_str!("../keymaps/nz.xkb"),
        "pk" => include_str!("../keymaps/pk.xkb"),
        "pl" => include_str!("../keymaps/pl.xkb"),
        "pt" => include_str!("../keymaps/pt.xkb"),
        "ro" => include_str!("../keymaps/ro.xkb"),
        "rs" => include_str!("../keymaps/rs.xkb"),
        "rs-latin" => include_str!("../keymaps/rs-latin.xkb"),
        "ru" => include_str!("../keymaps/ru.xkb"),
        "ru-cv" => include_str!("../keymaps/ru-cv.xkb"),
        "ru-phonetic" => include_str!("../keymaps/ru-phonetic.xkb"),
        "ru-sah" => include_str!("../keymaps/ru-sah.xkb"),
        "se" => include_str!("../keymaps/se.xkb"),
        "si" => include_str!("../keymaps/si.xkb"),
        "sk" => include_str!("../keymaps/sk.xkb"),
        "sk-qwerty" => include_str!("../keymaps/sk-qwerty.xkb"),
        "th" => include_str!("../keymaps/th.xkb"),
        "tj" => include_str!("../keymaps/tj.xkb"),
        "tm" => include_str!("../keymaps/tm.xkb"),
        "tr" => include_str!("../keymaps/tr.xkb"),
        "tr-f" => include_str!("../keymaps/tr-f.xkb"),
        "tr-ku" => include_str!("../keymaps/tr-ku.xkb"),
        "ua" => include_str!("../keymaps/ua.xkb"),
        "us" => include_str!("../keymaps/us.xkb"),
        "us-colemak" => include_str!("../keymaps/us-colemak.xkb"),
        "us-dvorak" => include_str!("../keymaps/us-dvorak.xkb"),
        "us-intl" => include_str!("../keymaps/us-intl.xkb"),
        "uz" => include_str!("../keymaps/uz.xkb"),
        "vn" => include_str!("../keymaps/vn.xkb"),
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
