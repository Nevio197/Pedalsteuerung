use crate::model::ActionPhase;
use objc2_app_kit::NSWorkspace;
use std::{
    collections::HashMap,
    ffi::c_void,
    ptr::null,
    sync::{LazyLock, Mutex},
};

type CfRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventCreateKeyboardEvent(source: CfRef, key: u16, down: bool) -> CfRef;
    fn CGEventSetFlags(event: CfRef, flags: u64);
    fn CGEventPost(tap: u32, event: CfRef);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: CfRef);
    fn CFDataGetBytePtr(data: CfRef) -> *const u8;
}
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> CfRef;
    fn TISGetInputSourceProperty(source: CfRef, key: CfRef) -> CfRef;
    static kTISPropertyUnicodeKeyLayoutData: CfRef;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const u8,
        key: u16,
        action: u16,
        modifier: u32,
        keyboard_type: u32,
        options: u32,
        dead_key: *mut u32,
        max_chars: u32,
        chars: *mut u32,
        out: *mut u16,
    ) -> i32;
}

struct OwnedCf(CfRef);

impl Drop for OwnedCf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

pub(super) fn frontmost_bundle_identifier() -> Option<String> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.bundleIdentifier())
        .map(|identifier| identifier.to_string())
}

pub(super) fn shortcut_permission() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

pub(super) fn request_shortcut_permission() -> bool {
    unsafe { CGRequestPostEventAccess() }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Modifier {
    Control,
    Option,
    Shift,
    Command,
}

const MODIFIERS: [Modifier; 4] = [
    Modifier::Control,
    Modifier::Option,
    Modifier::Shift,
    Modifier::Command,
];

impl Modifier {
    fn keys(self) -> (u16, u16) {
        match self {
            Self::Control => (0x3b, 0x3e),
            Self::Option => (0x3a, 0x3d),
            Self::Shift => (0x38, 0x3c),
            Self::Command => (0x37, 0x36),
        }
    }

    fn flags(self) -> u64 {
        match self {
            Self::Shift => 1 << 17,
            Self::Control => 1 << 18,
            Self::Option => 1 << 19,
            Self::Command => 1 << 20,
        }
    }
}

#[derive(Clone, Copy)]
struct Key {
    code: u16,
    character: Option<char>,
}

const CHARACTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const CHARACTER_CODES: [u16; 36] = [
    0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f, 0x23,
    0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06, 0x1d, 0x12, 0x13, 0x14, 0x15, 0x17,
    0x16, 0x1a, 0x1c, 0x19,
];
const FUNCTION_CODES: [u16; 20] = [
    0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71, 0x6a,
    0x40, 0x4f, 0x50, 0x5a,
];

fn parse_key(token: &str) -> Option<Key> {
    let code = match token {
        "enter" | "return" | "eingabe" => 0x24,
        "tab" => 0x30,
        "space" | "leertaste" => 0x31,
        "backspace" => 0x33,
        "esc" | "escape" => 0x35,
        "insert" | "einfg" => 0x72,
        "home" | "pos1" => 0x73,
        "pageup" | "bildauf" => 0x74,
        "delete" | "entf" => 0x75,
        "end" | "ende" => 0x77,
        "pagedown" | "bildab" => 0x79,
        "left" | "links" | "arrowleft" => 0x7b,
        "right" | "rechts" | "arrowright" => 0x7c,
        "down" | "runter" | "arrowdown" => 0x7d,
        "up" | "hoch" | "arrowup" => 0x7e,
        _ if token.len() == 1 => {
            let character = token.as_bytes()[0].to_ascii_uppercase() as char;
            let code = CHARACTER_CODES[CHARACTERS.find(character)?];
            return Some(Key {
                code,
                character: Some(character),
            });
        }
        _ => {
            let number = token
                .strip_prefix('f')?
                .parse::<usize>()
                .ok()?
                .checked_sub(1)?;
            *FUNCTION_CODES.get(number)?
        }
    };
    Some(Key {
        code,
        character: None,
    })
}

fn parse(shortcut: &str) -> Result<(Vec<Modifier>, Key), String> {
    let mut modifiers = Vec::new();
    let mut key = None;
    for part in shortcut.split('+') {
        let token = part.trim().to_lowercase().replace(' ', "");
        let modifier = match token.as_str() {
            "ctrl" | "control" | "strg" => Some(Modifier::Control),
            "alt" | "option" | "wahl" => Some(Modifier::Option),
            "shift" | "umschalt" => Some(Modifier::Shift),
            "command" | "cmd" => Some(Modifier::Command),
            _ => None,
        };
        if let Some(modifier) = modifier {
            if modifiers.contains(&modifier) {
                return Err("Ein Modifikator wurde doppelt angegeben.".into());
            }
            modifiers.push(modifier);
        } else if key.is_some() {
            return Err("Das Kürzel braucht genau eine Haupttaste.".into());
        } else {
            key = Some(parse_key(&token).ok_or("Unbekannte Taste. Beispiel: Ctrl+Shift+F9.")?);
        }
    }
    Ok((modifiers, key.ok_or("Das Kürzel braucht eine Haupttaste.")?))
}

fn layout_key(key: Key) -> u16 {
    let Some(character) = key.character else {
        return key.code;
    };
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return key.code;
        }
        let source = OwnedCf(source);
        let data = TISGetInputSourceProperty(source.0, kTISPropertyUnicodeKeyLayoutData);
        if data.is_null() {
            return key.code;
        }
        let layout = CFDataGetBytePtr(data);
        if layout.is_null() {
            return key.code;
        }
        let keyboard_type = u32::from(LMGetKbdType());
        for modifier in [0, 2] {
            for code in [key.code].into_iter().chain(CHARACTER_CODES) {
                let mut count = 0;
                let mut dead_key = 0;
                let mut buffer = [0; 4];
                let result = UCKeyTranslate(
                    layout,
                    code,
                    3,
                    modifier,
                    keyboard_type,
                    1,
                    &mut dead_key,
                    buffer.len() as u32,
                    &mut count,
                    buffer.as_mut_ptr(),
                );
                if result == 0
                    && count == 1
                    && buffer[0] <= 127
                    && (buffer[0] as u8 as char).eq_ignore_ascii_case(&character)
                {
                    return code;
                }
            }
        }
    }
    key.code
}

fn key_down(code: u16) -> bool {
    unsafe { CGEventSourceKeyState(1, code) }
}

fn pressed_modifiers() -> Vec<Modifier> {
    MODIFIERS
        .into_iter()
        .filter(|modifier| {
            let (left, right) = modifier.keys();
            key_down(left) || key_down(right)
        })
        .collect()
}

type KeyEvent = (u16, bool, u64);

fn post(events: impl IntoIterator<Item = KeyEvent>) -> Result<(), String> {
    let prepared: Result<Vec<_>, _> = events
        .into_iter()
        .map(|(code, down, flags)| {
            let event = unsafe { CGEventCreateKeyboardEvent(null(), code, down) };
            if event.is_null() {
                return Err("macOS konnte das Tastaturereignis nicht erstellen.".to_owned());
            }
            unsafe { CGEventSetFlags(event, flags) };
            Ok(OwnedCf(event))
        })
        .collect();
    for event in prepared? {
        unsafe { CGEventPost(0, event.0) };
    }
    Ok(())
}

struct HeldChord {
    key: u16,
    modifiers: Vec<Modifier>,
}

static HELD: LazyLock<Mutex<HashMap<String, HeldChord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn release_events(chord: &HeldChord, existing_flags: u64) -> Vec<KeyEvent> {
    let mut flags = existing_flags
        | chord
            .modifiers
            .iter()
            .fold(0, |bits, item| bits | item.flags());
    let mut events = vec![(chord.key, false, flags)];
    for modifier in chord.modifiers.iter().rev() {
        flags &= !modifier.flags();
        flags |= existing_flags & modifier.flags();
        events.push((modifier.keys().0, false, flags));
    }
    events
}

pub(super) fn keyboard(shortcut: &str, phase: ActionPhase) -> Result<(), String> {
    let mut held = HELD
        .lock()
        .map_err(|_| "Tastaturzustand konnte nicht gelesen werden.")?;
    if matches!(phase, ActionPhase::Release) {
        if let Some(chord) = held.remove(shortcut) {
            let existing = pressed_modifiers().into_iter().fold(0, |bits, item| {
                let external = !chord.modifiers.contains(&item) || key_down(item.keys().1);
                if external { bits | item.flags() } else { bits }
            });
            return post(release_events(&chord, existing));
        }
        return Ok(());
    }

    if !shortcut_permission() {
        return Err(
            "Erlaube Pedalsteuerung 2 unter Datenschutz & Sicherheit → Bedienungshilfen.".into(),
        );
    }
    if !held.is_empty() {
        return Err("Ein Pedal hält bereits ein Tastenkürzel. Lass es zuerst los.".into());
    }
    let (modifiers, key) = parse(shortcut)?;
    let key = layout_key(key);
    let existing = pressed_modifiers();
    if key_down(key) || existing.iter().any(|item| !modifiers.contains(item)) {
        return Err(
            "Andere Tasten sind gerade gedrückt. Versuche es nach dem Loslassen erneut.".into(),
        );
    }
    let existing_flags = existing.iter().fold(0, |bits, item| bits | item.flags());
    let mut flags = existing_flags;
    let modifiers: Vec<_> = modifiers
        .into_iter()
        .filter(|item| !existing.contains(item))
        .collect();
    let mut events: Vec<_> = modifiers
        .iter()
        .map(|modifier| {
            flags |= modifier.flags();
            (modifier.keys().0, true, flags)
        })
        .collect();
    events.push((key, true, flags));
    let chord = HeldChord { key, modifiers };
    if matches!(phase, ActionPhase::Pulse) {
        events.extend(release_events(&chord, existing_flags));
        return post(events);
    }
    post(events)?;
    held.insert(shortcut.to_owned(), chord);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chords_and_rejects_duplicate_keys() {
        let (mods, key) = parse("Strg + Umschalt + F9").unwrap();
        assert_eq!(mods.len(), 2);
        assert_eq!(key.code, 0x65);
        assert!(parse("Shift+Shift+A").is_err());
        assert!(parse("Shift+A+B").is_err());
    }

    #[test]
    fn release_preserves_existing_modifiers() {
        let chord = HeldChord {
            key: 0x65,
            modifiers: vec![Modifier::Shift],
        };
        let events = release_events(&chord, Modifier::Control.flags());
        assert_eq!(
            events,
            [
                (
                    0x65,
                    false,
                    Modifier::Shift.flags() | Modifier::Control.flags()
                ),
                (Modifier::Shift.keys().0, false, Modifier::Control.flags()),
            ]
        );
    }
}
