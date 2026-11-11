//! Turn a key event into the chord name the keymap uses, for the shortcut editor's "press the
//! keys" capture. The rules mirror the shell's own key handler (`main.rs`), so a shortcut recorded
//! here is the one the dispatcher will see later:
//! * AltGr (Windows reports Ctrl+Alt) never forms a letter chord, it types characters;
//! * Ctrl+digit follows the PHYSICAL top-row key (AZERTY types `&` `é` `"` ... unshifted);
//! * a real Ctrl+Alt+letter arrives as an unidentified Windows virtual key, mapped back to the letter.

use brouillon_core::keymap::KeyCombo;
use floem::keyboard::{Key, KeyCode, NamedKey, NativeKey, PhysicalKey};

fn digit_of(physical: &PhysicalKey) -> Option<&'static str> {
    let PhysicalKey::Code(code) = physical else { return None };
    Some(match code {
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::Digit0 => "0",
        _ => return None,
    })
}

/// Name of the key in a chord (`f3`, `pagedown`, `a`...). `None` for modifiers and keys that never form a chord.
fn key_name(key: &Key) -> Option<String> {
    let named = match key {
        Key::Character(c) => return Some(c.to_lowercase()),
        Key::Unidentified(NativeKey::Windows(vk)) if (0x41..=0x5A).contains(vk) => {
            return Some(((*vk as u8 + 32) as char).to_string());
        }
        Key::Named(n) => match n {
            NamedKey::ArrowLeft => "left",
            NamedKey::ArrowRight => "right",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::Enter => "enter",
            NamedKey::Tab => "tab",
            NamedKey::Escape => "escape",
            NamedKey::Backspace => "backspace",
            NamedKey::Delete => "delete",
            NamedKey::Insert => "insert",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Space => "space",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            NamedKey::F5 => "f5",
            NamedKey::F6 => "f6",
            NamedKey::F7 => "f7",
            NamedKey::F8 => "f8",
            NamedKey::F9 => "f9",
            NamedKey::F10 => "f10",
            NamedKey::F11 => "f11",
            NamedKey::F12 => "f12",
            _ => return None,
        },
        _ => return None,
    };
    Some(named.to_string())
}

/// The chord step for a key press, or `None` when it is only a modifier / not chord material.
pub fn combo_from_parts(logical: &Key, physical: &PhysicalKey, ctrl: bool, shift: bool, alt: bool, altgr: bool) -> Option<KeyCombo> {
    // AltGr + key types a character; it is not recordable as a shortcut.
    if altgr {
        return None;
    }
    let name = if ctrl {
        digit_of(physical).map(str::to_string).or_else(|| key_name(logical))?
    } else {
        key_name(logical)?
    };
    Some(KeyCombo::new(ctrl, shift, alt, &name))
}

/// A shortcut must not be a bare typing key: it needs Ctrl or Alt, or be a function key.
pub fn is_usable(c: &KeyCombo) -> bool {
    let function_key = c.key.len() >= 2 && c.key.starts_with('f') && c.key[1..].chars().all(|ch| ch.is_ascii_digit());
    c.ctrl || c.alt || function_key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(c: Option<KeyCombo>) -> String {
        c.map(|c| c.to_string_repr()).unwrap_or_default()
    }

    #[test]
    fn letters_function_keys_and_modifiers() {
        let key = |c: &str| Key::Character(c.into());
        let none = PhysicalKey::Code(KeyCode::KeyA);
        assert_eq!(s(combo_from_parts(&key("S"), &none, true, true, false, false)), "ctrl+shift+s");
        assert_eq!(s(combo_from_parts(&Key::Named(NamedKey::F5), &none, false, false, false, false)), "f5");
        assert_eq!(s(combo_from_parts(&Key::Named(NamedKey::PageDown), &none, true, false, false, false)), "ctrl+pagedown");
        // A modifier on its own is not a shortcut.
        assert_eq!(s(combo_from_parts(&Key::Named(NamedKey::Control), &none, true, false, false, false)), "");
        assert_eq!(s(combo_from_parts(&Key::Named(NamedKey::Shift), &none, false, true, false, false)), "");
        assert_eq!(s(combo_from_parts(&Key::Named(NamedKey::Alt), &none, false, false, true, false)), "");
    }

    #[test]
    fn ctrl_digit_follows_the_physical_key_and_altgr_is_not_a_chord() {
        let phys = PhysicalKey::Code(KeyCode::Digit3);
        // AZERTY: the unshifted key types `"`.
        assert_eq!(s(combo_from_parts(&Key::Character("\"".into()), &phys, true, false, false, false)), "ctrl+3");
        assert_eq!(s(combo_from_parts(&Key::Character("\"".into()), &phys, true, false, true, false)), "ctrl+alt+3");
        // AltGr+key types a character: it is never recorded as Ctrl+Alt+digit.
        assert_eq!(s(combo_from_parts(&Key::Character("#".into()), &phys, true, false, true, true)), "");
        assert_eq!(s(combo_from_parts(&Key::Unidentified(NativeKey::Windows(0x41)), &phys, true, false, true, true)), "");
    }

    #[test]
    fn real_ctrl_alt_letter_arrives_as_a_windows_virtual_key() {
        let none = PhysicalKey::Code(KeyCode::KeyA);
        assert_eq!(s(combo_from_parts(&Key::Unidentified(NativeKey::Windows(0x41)), &none, true, false, true, false)), "ctrl+alt+a");
        assert_eq!(s(combo_from_parts(&Key::Unidentified(NativeKey::Windows(0x70)), &none, true, false, true, false)), "");
    }

    #[test]
    fn bare_typing_keys_are_not_usable_shortcuts() {
        assert!(is_usable(&KeyCombo::new(true, false, false, "p")));
        assert!(is_usable(&KeyCombo::new(false, false, true, "x")));
        assert!(is_usable(&KeyCombo::new(false, false, false, "f12")));
        assert!(!is_usable(&KeyCombo::new(false, false, false, "a")));
        assert!(!is_usable(&KeyCombo::new(false, true, false, "a")));
        assert!(!is_usable(&KeyCombo::new(false, false, false, "fx")));
    }
}
