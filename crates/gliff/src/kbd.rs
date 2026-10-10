//! The macOS keyboard translation: GDK reports the Carbon virtual key
//! codes (`kVK_*`, `NSEvent.keyCode`), while the server wants Linux
//! evdev codes. The mapping is purely positional — both number the
//! same physical keys — so a table suffices, and the shipped xkb
//! keymap (see `keymap`) does the layout work.
#![cfg(target_os = "macos")]

/// The evdev code for a `kVK_*` virtual keycode, or `None` when there
/// is no sensible evdev counterpart (media keys macOS handles itself,
/// bare function keys, unknown codes).
pub fn evdev_code(key: u16) -> Option<u32> {
    Some(match key {
        // ANSI letters.
        0x00 => 30, // A
        0x0B => 48, // B
        0x08 => 46, // C
        0x02 => 32, // D
        0x0E => 18, // E
        0x03 => 33, // F
        0x05 => 34, // G
        0x04 => 35, // H
        0x22 => 23, // I
        0x26 => 36, // J
        0x28 => 37, // K
        0x25 => 38, // L
        0x2D => 49, // N
        0x1F => 24, // O
        0x23 => 25, // P
        0x0C => 16, // Q
        0x0F => 19, // R
        0x01 => 31, // S
        0x11 => 20, // T
        0x2E => 50, // M
        0x09 => 47, // V
        0x0D => 17, // W
        0x07 => 45, // X
        0x10 => 21, // Y
        0x06 => 44, // Z
        // ANSI digits row, left to right.
        0x12 => 2,  // 1
        0x13 => 3,  // 2
        0x14 => 4,  // 3
        0x15 => 5,  // 4
        0x17 => 6,  // 5
        0x16 => 7,  // 6
        0x1A => 8,  // 7
        0x1C => 9,  // 8
        0x19 => 10, // 9
        0x1D => 11, // 0
        // Punctuation.
        0x1B => 12, // -_
        0x18 => 13, // =+
        0x21 => 26, // [{
        0x1E => 27, // ]}
        0x29 => 39, // ;:  (KEY_SEMICOLON)
        0x27 => 40, // '"  (KEY_APOSTROPHE)
        0x2A => 43, // \|
        0x2B => 51, // ,<
        0x2F => 52, // .>
        0x2C => 53, // /?
        0x32 => 41, // `~
        // Whitespace and edit.
        0x24 => 28,  // Return
        0x30 => 15,  // Tab
        0x31 => 57,  // Space
        0x33 => 14,  // Delete = backspace
        0x35 => 1,   // Escape
        0x75 => 111, // Forward delete
        0x73 => 102, // Home
        0x77 => 107, // End
        0x74 => 104, // PageUp
        0x79 => 109, // PageDown
        0x7B => 105, // Left
        0x7C => 106, // Right
        0x7D => 108, // Down
        0x7E => 103, // Up
        0x72 => 110, // Help = Insert
        0x6E => 127, // Contextual menu = Compose
        // Modifiers. Command is the Mac super key; both are sent so
        // remote shortcuts can use either physical key.
        0x37 => 125, // Command = Left meta
        0x36 => 126, // Right command = Right meta
        0x38 => 42,  // Shift
        0x3C => 54,  // Right shift
        0x3A => 56,  // Option = Left alt
        0x3D => 100, // Right option = Right alt
        0x3B => 29,  // Control
        0x3E => 97,  // Right control
        0x39 => 58,  // Caps lock
        // Function row.
        0x7A => 59,  // F1
        0x78 => 60,  // F2
        0x63 => 61,  // F3
        0x76 => 62,  // F4
        0x60 => 63,  // F5
        0x61 => 64,  // F6
        0x62 => 65,  // F7
        0x64 => 66,  // F8
        0x65 => 67,  // F9
        0x6D => 68,  // F10
        0x67 => 87,  // F11
        0x6F => 88,  // F12
        0x69 => 89,  // F13
        0x6B => 90,  // F14
        0x71 => 91,  // F15
        0x6A => 92,  // F16
        0x40 => 93,  // F17
        0x4F => 94,  // F18
        0x50 => 95,  // F19
        0x5A => 190, // F20
        // Keypad. Clear doubles as num lock; the codes are the evdev
        // keypad block so the keymap's KP symbols apply.
        0x4B => 98,  // /
        0x43 => 55,  // *
        0x45 => 78,  // +
        0x4E => 74,  // -
        0x4C => 96,  // Enter
        0x51 => 117, // =
        0x41 => 83,  // .
        0x47 => 69,  // Clear = num lock
        0x52 => 82,  // 0
        0x53 => 79,  // 1
        0x54 => 80,  // 2
        0x55 => 81,  // 3
        0x56 => 75,  // 4
        0x57 => 76,  // 5
        0x58 => 77,  // 6
        0x59 => 71,  // 7
        0x5B => 72,  // 8
        0x5C => 73,  // 9
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::evdev_code;
    use std::collections::HashSet;

    /// Spot checks against well-known pairs.
    #[test]
    fn known_keys() {
        assert_eq!(evdev_code(0x00), Some(30)); // A
        assert_eq!(evdev_code(0x06), Some(44)); // Z
        assert_eq!(evdev_code(0x12), Some(2)); // 1
        assert_eq!(evdev_code(0x1D), Some(11)); // 0
        assert_eq!(evdev_code(0x24), Some(28)); // Return
        assert_eq!(evdev_code(0x33), Some(14)); // Backspace
        assert_eq!(evdev_code(0x75), Some(111)); // Forward delete
        assert_eq!(evdev_code(0x37), Some(125)); // Command = super
        assert_eq!(evdev_code(0x3A), Some(56)); // Option = alt
        assert_eq!(evdev_code(0x7A), Some(59)); // F1
        assert_eq!(evdev_code(0x7E), Some(103)); // Up
        assert_eq!(evdev_code(0x52), Some(82)); // KP 0
        assert_eq!(evdev_code(0x4C), Some(96)); // KP enter
        assert_eq!(evdev_code(0x48), None); // Volume up: macOS keeps it
        assert_eq!(evdev_code(0x3F), None); // Bare fn
        assert_eq!(evdev_code(0xFF), None); // Nothing there
    }

    /// Every mapped code is a real evdev code (< KEY_MAX = 0x2ff) and
    /// no two keys collapse onto the same code, so no press can be
    /// released by another key's event.
    #[test]
    fn distinct_and_in_range() {
        let mut seen = HashSet::new();
        for key in 0..=0x7Fu16 {
            if let Some(code) = evdev_code(key) {
                assert!(code < 0x2ff, "{key:#x} -> {code}");
                assert!(seen.insert(code), "duplicate code {code} for {key:#x}");
            }
        }
    }
}
