use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press(u8),
    Release(u8),
    Motion(u8),
    WheelUp,
    WheelDown,
}

pub fn encode_key(
    event: &KeyEvent,
    modifiers: ModifiersState,
    application_cursor_keys: bool,
    application_keypad: bool,
) -> Option<Vec<u8>> {
    if event.state == ElementState::Pressed
        && application_keypad
        && modifiers.is_empty()
        && let Some(bytes) = encode_application_keypad(&event.physical_key)
    {
        return Some(bytes);
    }
    encode(
        event.state,
        &event.logical_key,
        event.text.as_deref(),
        modifiers,
        application_cursor_keys,
    )
}

fn encode_application_keypad(key: &PhysicalKey) -> Option<Vec<u8>> {
    let final_byte = match key {
        PhysicalKey::Code(KeyCode::Numpad0) => b'p',
        PhysicalKey::Code(KeyCode::Numpad1) => b'q',
        PhysicalKey::Code(KeyCode::Numpad2) => b'r',
        PhysicalKey::Code(KeyCode::Numpad3) => b's',
        PhysicalKey::Code(KeyCode::Numpad4) => b't',
        PhysicalKey::Code(KeyCode::Numpad5) => b'u',
        PhysicalKey::Code(KeyCode::Numpad6) => b'v',
        PhysicalKey::Code(KeyCode::Numpad7) => b'w',
        PhysicalKey::Code(KeyCode::Numpad8) => b'x',
        PhysicalKey::Code(KeyCode::Numpad9) => b'y',
        PhysicalKey::Code(KeyCode::NumpadDecimal) => b'n',
        PhysicalKey::Code(KeyCode::NumpadDivide) => b'o',
        PhysicalKey::Code(KeyCode::NumpadMultiply) => b'j',
        PhysicalKey::Code(KeyCode::NumpadSubtract) => b'm',
        PhysicalKey::Code(KeyCode::NumpadAdd) => b'k',
        PhysicalKey::Code(KeyCode::NumpadEnter) => b'M',
        PhysicalKey::Code(KeyCode::NumpadEqual) => b'X',
        _ => return None,
    };
    Some(vec![0x1b, b'O', final_byte])
}

fn encode(
    state: ElementState,
    key: &Key,
    text: Option<&str>,
    modifiers: ModifiersState,
    application_cursor_keys: bool,
) -> Option<Vec<u8>> {
    if state != ElementState::Pressed {
        return None;
    }

    let special_bytes = encode_special_key(key, modifiers, application_cursor_keys);
    let special_key = special_bytes.is_some();
    let mut bytes = if let Some(bytes) = special_bytes {
        bytes
    } else if modifiers.control_key() {
        encode_control(key)?
    } else {
        match key {
            Key::Named(NamedKey::Enter) => vec![b'\r'],
            Key::Named(NamedKey::Backspace) => vec![0x7f],
            Key::Named(NamedKey::Tab) => vec![b'\t'],
            Key::Named(NamedKey::Escape) => vec![0x1b],
            _ => text?.as_bytes().to_vec(),
        }
    };

    if modifiers.alt_key() && !special_key && !matches!(key, Key::Named(NamedKey::Escape)) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn encode_special_key(
    key: &Key,
    modifiers: ModifiersState,
    application_cursor_keys: bool,
) -> Option<Vec<u8>> {
    let modifier = xterm_modifier(modifiers);
    let final_byte = match key {
        Key::Named(NamedKey::ArrowUp) => b'A',
        Key::Named(NamedKey::ArrowDown) => b'B',
        Key::Named(NamedKey::ArrowRight) => b'C',
        Key::Named(NamedKey::ArrowLeft) => b'D',
        Key::Named(NamedKey::Home) => b'H',
        Key::Named(NamedKey::End) => b'F',
        _ => 0,
    };
    if final_byte != 0 {
        if modifier == 1 {
            return Some(if application_cursor_keys {
                vec![0x1b, b'O', final_byte]
            } else {
                vec![0x1b, b'[', final_byte]
            });
        }
        return Some(format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes());
    }

    let tilde_code = match key {
        Key::Named(NamedKey::Insert) => 2,
        Key::Named(NamedKey::Delete) => 3,
        Key::Named(NamedKey::PageUp) => 5,
        Key::Named(NamedKey::PageDown) => 6,
        Key::Named(NamedKey::F5) => 15,
        Key::Named(NamedKey::F6) => 17,
        Key::Named(NamedKey::F7) => 18,
        Key::Named(NamedKey::F8) => 19,
        Key::Named(NamedKey::F9) => 20,
        Key::Named(NamedKey::F10) => 21,
        Key::Named(NamedKey::F11) => 23,
        Key::Named(NamedKey::F12) => 24,
        _ => 0,
    };
    if tilde_code != 0 {
        return Some(if modifier == 1 {
            format!("\x1b[{tilde_code}~").into_bytes()
        } else {
            format!("\x1b[{tilde_code};{modifier}~").into_bytes()
        });
    }

    let function_final = match key {
        Key::Named(NamedKey::F1) => b'P',
        Key::Named(NamedKey::F2) => b'Q',
        Key::Named(NamedKey::F3) => b'R',
        Key::Named(NamedKey::F4) => b'S',
        _ => 0,
    };
    if function_final != 0 {
        return Some(if modifier == 1 {
            vec![0x1b, b'O', function_final]
        } else {
            format!("\x1b[1;{modifier}{}", function_final as char).into_bytes()
        });
    }

    if matches!(key, Key::Named(NamedKey::Tab)) && modifiers.shift_key() {
        return Some(if modifier == 2 {
            b"\x1b[Z".to_vec()
        } else {
            format!("\x1b[1;{modifier}Z").into_bytes()
        });
    }
    None
}

fn xterm_modifier(modifiers: ModifiersState) -> u8 {
    1 + u8::from(modifiers.shift_key())
        + 2 * u8::from(modifiers.alt_key())
        + 4 * u8::from(modifiers.control_key())
        + 8 * u8::from(modifiers.super_key())
}

pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }
    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

pub fn encode_mouse(
    kind: MouseEventKind,
    row: usize,
    column: usize,
    modifiers: ModifiersState,
    sgr: bool,
) -> Option<Vec<u8>> {
    let (button, released, motion) = match kind {
        MouseEventKind::Press(button) => (button, false, false),
        MouseEventKind::Release(button) => (button, true, false),
        MouseEventKind::Motion(button) => (button, false, true),
        MouseEventKind::WheelUp => (64, false, false),
        MouseEventKind::WheelDown => (65, false, false),
    };
    let modifier = 4 * u8::from(modifiers.shift_key())
        + 8 * u8::from(modifiers.alt_key())
        + 16 * u8::from(modifiers.control_key());
    let code = button + modifier + 32 * u8::from(motion);
    let row = row.saturating_add(1);
    let column = column.saturating_add(1);
    if sgr {
        return Some(
            format!(
                "\x1b[<{code};{column};{row}{}",
                if released { 'm' } else { 'M' }
            )
            .into_bytes(),
        );
    }

    let column = u8::try_from(column).ok()?.checked_add(32)?;
    let row = u8::try_from(row).ok()?.checked_add(32)?;
    Some(vec![
        0x1b,
        b'[',
        b'M',
        if released { 3 + modifier } else { code } + 32,
        column,
        row,
    ])
}

fn encode_control(key: &Key) -> Option<Vec<u8>> {
    match key {
        Key::Character(value) => {
            let byte = value.as_bytes().first().copied()?;
            let byte = byte.to_ascii_uppercase();
            match byte {
                b'@'..=b'_' => Some(vec![byte & 0x1f]),
                b'?' => Some(vec![0x7f]),
                _ => None,
            }
        }
        Key::Named(NamedKey::Enter) => Some(vec![b'\n']),
        Key::Named(NamedKey::Backspace) => Some(vec![0x08]),
        Key::Named(NamedKey::Tab) => Some(vec![b'\t']),
        Key::Named(NamedKey::Space) => Some(vec![0]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use winit::{
        event::ElementState,
        keyboard::{Key, ModifiersState, NamedKey},
    };

    use super::{MouseEventKind, encode, encode_application_keypad, encode_mouse, encode_paste};
    use crate::terminal::{MouseTracking, Terminal, TerminalParser};

    fn modifiers(control: bool, alt: bool, shift: bool) -> ModifiersState {
        let mut modifiers = ModifiersState::empty();
        modifiers.set(ModifiersState::CONTROL, control);
        modifiers.set(ModifiersState::ALT, alt);
        modifiers.set(ModifiersState::SHIFT, shift);
        modifiers
    }

    #[test]
    fn encodes_ctrl_letters_as_ascii_control_bytes() {
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Character("c".into()),
                Some("c"),
                modifiers(true, false, false),
                false,
            ),
            Some(vec![0x03])
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Character("d".into()),
                Some("d"),
                modifiers(true, false, false),
                false,
            ),
            Some(vec![0x04])
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Character("z".into()),
                Some("z"),
                modifiers(true, false, false),
                false,
            ),
            Some(vec![0x1a])
        );
    }

    #[test]
    fn encodes_navigation_and_editing_keys() {
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::ArrowUp),
                None,
                ModifiersState::empty(),
                false,
            ),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::Home),
                None,
                ModifiersState::empty(),
                false,
            ),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::Delete),
                None,
                ModifiersState::empty(),
                false,
            ),
            Some(b"\x1b[3~".to_vec())
        );
    }

    #[test]
    fn prefixes_alt_modified_text_with_escape() {
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Character("x".into()),
                Some("x"),
                modifiers(false, true, false),
                false,
            ),
            Some(vec![0x1b, b'x'])
        );
    }

    #[test]
    fn wraps_bracketed_paste_when_requested() {
        assert_eq!(encode_paste("hello", false), b"hello");
        assert_eq!(encode_paste("hello", true), b"\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn encodes_application_cursor_and_modified_navigation_keys() {
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::ArrowUp),
                None,
                ModifiersState::empty(),
                true,
            ),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::ArrowLeft),
                None,
                modifiers(true, false, true),
                false,
            ),
            Some(b"\x1b[1;6D".to_vec())
        );
    }

    #[test]
    fn encodes_function_keys() {
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::F1),
                None,
                ModifiersState::empty(),
                false,
            ),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::F12),
                None,
                modifiers(false, true, false),
                false,
            ),
            Some(b"\x1b[24;3~".to_vec())
        );
    }

    #[test]
    fn encodes_sgr_and_legacy_mouse_events() {
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press(0),
                1,
                2,
                ModifiersState::empty(),
                true,
            ),
            Some(b"\x1b[<0;3;2M".to_vec())
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Release(0),
                1,
                2,
                ModifiersState::empty(),
                false,
            ),
            Some(vec![0x1b, b'[', b'M', 35, 35, 34])
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Motion(0),
                4,
                9,
                ModifiersState::empty(),
                true,
            ),
            Some(b"\x1b[<32;10;5M".to_vec())
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::WheelDown,
                4,
                9,
                ModifiersState::empty(),
                true,
            ),
            Some(b"\x1b[<65;10;5M".to_vec())
        );
    }

    #[test]
    fn mouse_reporting_encoding_does_not_mutate_terminal_cursor() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(3, 12);
        parser.process(&mut terminal, b"echo");
        let cursor = terminal.render_snapshot().cursor;
        parser.process(&mut terminal, b"\x1b[?1000;1006h");
        assert_eq!(terminal.mouse_tracking(), MouseTracking::Button);
        assert!(terminal.sgr_mouse());

        let encoded = encode_mouse(
            MouseEventKind::Press(0),
            1,
            7,
            ModifiersState::empty(),
            terminal.sgr_mouse(),
        );

        assert_eq!(encoded, Some(b"\x1b[<0;8;2M".to_vec()));
        assert_eq!(terminal.render_snapshot().cursor, cursor);
    }

    #[test]
    fn encodes_application_keypad_keys() {
        assert_eq!(
            encode_application_keypad(&winit::keyboard::PhysicalKey::Code(
                winit::keyboard::KeyCode::Numpad7,
            )),
            Some(b"\x1bOw".to_vec())
        );
    }
}
