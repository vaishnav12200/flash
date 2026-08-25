use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

pub fn encode_key(event: &KeyEvent, modifiers: ModifiersState) -> Option<Vec<u8>> {
    encode(
        event.state,
        &event.logical_key,
        event.text.as_deref(),
        modifiers,
    )
}

fn encode(
    state: ElementState,
    key: &Key,
    text: Option<&str>,
    modifiers: ModifiersState,
) -> Option<Vec<u8>> {
    if state != ElementState::Pressed {
        return None;
    }

    let mut bytes = if modifiers.control_key() {
        encode_control(key)?
    } else {
        match key {
            Key::Named(NamedKey::Enter) => vec![b'\r'],
            Key::Named(NamedKey::Backspace) => vec![0x7f],
            Key::Named(NamedKey::Tab) if modifiers.shift_key() => b"\x1b[Z".to_vec(),
            Key::Named(NamedKey::Tab) => vec![b'\t'],
            Key::Named(NamedKey::Escape) => vec![0x1b],
            Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
            Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
            Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
            Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
            Key::Named(NamedKey::Home) => b"\x1b[H".to_vec(),
            Key::Named(NamedKey::End) => b"\x1b[F".to_vec(),
            Key::Named(NamedKey::Insert) => b"\x1b[2~".to_vec(),
            Key::Named(NamedKey::Delete) => b"\x1b[3~".to_vec(),
            Key::Named(NamedKey::PageUp) => b"\x1b[5~".to_vec(),
            Key::Named(NamedKey::PageDown) => b"\x1b[6~".to_vec(),
            _ => text?.as_bytes().to_vec(),
        }
    };

    if modifiers.alt_key() && !matches!(key, Key::Named(NamedKey::Escape)) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
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

    use super::{encode, encode_paste};

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
                modifiers(true, false, false)
            ),
            Some(vec![0x03])
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Character("d".into()),
                Some("d"),
                modifiers(true, false, false)
            ),
            Some(vec![0x04])
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Character("z".into()),
                Some("z"),
                modifiers(true, false, false)
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
                ModifiersState::empty()
            ),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::Home),
                None,
                ModifiersState::empty()
            ),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode(
                ElementState::Pressed,
                &Key::Named(NamedKey::Delete),
                None,
                ModifiersState::empty()
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
                modifiers(false, true, false)
            ),
            Some(vec![0x1b, b'x'])
        );
    }

    #[test]
    fn wraps_bracketed_paste_when_requested() {
        assert_eq!(encode_paste("hello", false), b"hello");
        assert_eq!(encode_paste("hello", true), b"\x1b[200~hello\x1b[201~");
    }
}
