//! Global hotkey bindings for start/pause/stop control.
//!
//! Parsing is platform-neutral and testable everywhere; registration and the
//! message pump are Windows-only and live in `windows_backend`.

use serde::{Deserialize, Serialize};

/// What a registered chord should do when pressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HotkeyAction {
    Start,
    PauseResume,
    Stop,
    EmergencyStop,
}

/// A parsed chord in Win32 `RegisterHotKey` terms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HotkeyChord {
    /// Bitwise OR of `MOD_ALT`(1), `MOD_CONTROL`(2), `MOD_SHIFT`(4), `MOD_WIN`(8).
    pub modifiers: u32,
    pub virtual_key: u32,
}

const MOD_ALT: u32 = 0x1;
const MOD_CONTROL: u32 = 0x2;
const MOD_SHIFT: u32 = 0x4;
const MOD_WIN: u32 = 0x8;

/// Parses profile hotkey strings such as `"F1"` or `"Ctrl+Shift+F12"`.
/// Only function keys F1..=F24 are accepted as the terminal key so a typo can
/// never bind a movement letter globally.
#[must_use]
pub fn parse_hotkey(text: &str) -> Option<HotkeyChord> {
    let mut modifiers = 0_u32;
    let mut virtual_key = None;
    for part in text.split('+') {
        let part = part.trim();
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "shift" => modifiers |= MOD_SHIFT,
            "alt" => modifiers |= MOD_ALT,
            "win" | "super" => modifiers |= MOD_WIN,
            lower => {
                let number = lower.strip_prefix('f')?.parse::<u32>().ok()?;
                if !(1..=24).contains(&number) || virtual_key.is_some() {
                    return None;
                }
                virtual_key = Some(0x6F + number); // VK_F1 = 0x70
            }
        }
    }
    Some(HotkeyChord {
        modifiers,
        virtual_key: virtual_key?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_modified_function_keys() {
        assert_eq!(
            parse_hotkey("F1"),
            Some(HotkeyChord {
                modifiers: 0,
                virtual_key: 0x70,
            })
        );
        assert_eq!(
            parse_hotkey("Ctrl+Shift+F12"),
            Some(HotkeyChord {
                modifiers: MOD_CONTROL | MOD_SHIFT,
                virtual_key: 0x7B,
            })
        );
        assert_eq!(
            parse_hotkey("Alt+F24").map(|chord| chord.virtual_key),
            Some(0x87)
        );
    }

    #[test]
    fn refuses_non_function_terminal_keys() {
        assert_eq!(parse_hotkey("W"), None);
        assert_eq!(parse_hotkey("Ctrl+C"), None);
        assert_eq!(parse_hotkey("F25"), None);
        assert_eq!(parse_hotkey("Ctrl+Shift"), None);
        assert_eq!(parse_hotkey("F1+F2"), None);
    }
}
