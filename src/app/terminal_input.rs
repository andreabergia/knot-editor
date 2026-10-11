//! Legacy xterm keyboard encoding from the emulator's current modes.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

pub(crate) fn terminal_key_bytes(stroke: &Keystroke, mode: TermMode) -> Vec<u8> {
    if stroke.modifiers.platform {
        return Vec::new();
    }
    let modifiers = stroke.modifiers;
    let parameter = 1
        + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control);
    let key = stroke.key.to_lowercase();
    let final_byte = match key.as_str() {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    };
    if let Some(final_byte) = final_byte {
        return if parameter != 1 {
            format!("\x1b[1;{parameter}{final_byte}").into_bytes()
        } else if key.starts_with('f') || mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        };
    }
    let number = match key.as_str() {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(number) = number {
        return if parameter == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{parameter}~")
        }
        .into_bytes();
    }
    if key == "tab" && modifiers.shift {
        return if parameter == 2 {
            b"\x1b[Z".to_vec()
        } else {
            format!("\x1b[1;{parameter}Z").into_bytes()
        };
    }
    let mut bytes = match key.as_str() {
        "enter" => b"\r".to_vec(),
        "backspace" => vec![if modifiers.control { 0x08 } else { 0x7f }],
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        _ if modifiers.control => control_byte(&key).into_iter().collect(),
        "space" => vec![b' '],
        _ if modifiers.alt && key.len() == 1 && key.is_ascii() => if modifiers.shift {
            key.to_uppercase()
        } else {
            key.clone()
        }
        .into_bytes(),
        _ => stroke
            .key_char
            .as_deref()
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    };
    if modifiers.alt && !bytes.is_empty() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn control_byte(key: &str) -> Option<u8> {
    match key {
        "space" | "@" | "2" => Some(0),
        "[" | "3" => Some(0x1b),
        "\\" | "4" => Some(0x1c),
        "]" | "5" => Some(0x1d),
        "^" | "6" => Some(0x1e),
        "_" | "-" | "7" => Some(0x1f),
        "?" | "8" => Some(0x7f),
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_lowercase() => {
            Some(key.as_bytes()[0] - b'a' + 1)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_function_and_navigation_modes_and_every_modifier() {
        for app_cursor in [false, true] {
            let mode = if app_cursor {
                TermMode::APP_CURSOR
            } else {
                TermMode::NONE
            };
            for (key, suffix) in [
                ("up", 'A'),
                ("down", 'B'),
                ("right", 'C'),
                ("left", 'D'),
                ("home", 'H'),
                ("end", 'F'),
                ("f1", 'P'),
                ("f2", 'Q'),
                ("f3", 'R'),
                ("f4", 'S'),
            ] {
                for bits in 0..8 {
                    let stroke = Keystroke {
                        key: key.into(),
                        modifiers: gpui::Modifiers {
                            shift: bits & 1 != 0,
                            alt: bits & 2 != 0,
                            control: bits & 4 != 0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let expected = if bits != 0 {
                        format!("\x1b[1;{}{suffix}", bits + 1)
                    } else if app_cursor || key.starts_with('f') {
                        format!("\x1bO{suffix}")
                    } else {
                        format!("\x1b[{suffix}")
                    };
                    assert_eq!(
                        terminal_key_bytes(&stroke, mode),
                        expected.as_bytes(),
                        "{key}, modifiers {bits}"
                    );
                }
            }
        }
        for (key, number) in [
            ("insert", 2),
            ("delete", 3),
            ("pageup", 5),
            ("pagedown", 6),
            ("f5", 15),
            ("f6", 17),
            ("f7", 18),
            ("f8", 19),
            ("f9", 20),
            ("f10", 21),
            ("f11", 23),
            ("f12", 24),
        ] {
            for bits in 0..8 {
                let stroke = Keystroke {
                    key: key.into(),
                    modifiers: gpui::Modifiers {
                        shift: bits & 1 != 0,
                        alt: bits & 2 != 0,
                        control: bits & 4 != 0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let expected = if bits == 0 {
                    format!("\x1b[{number}~")
                } else {
                    format!("\x1b[{number};{}~", bits + 1)
                };
                assert_eq!(
                    terminal_key_bytes(&stroke, TermMode::APP_CURSOR),
                    expected.as_bytes()
                );
            }
        }
    }

    #[test]
    fn text_controls_and_unrecognized_keys() {
        for (key, byte) in [
            ("space", 0),
            ("2", 0),
            ("a", 1),
            ("z", 26),
            ("[", 27),
            ("\\", 28),
            ("]", 29),
            ("^", 30),
            ("_", 31),
            ("?", 127),
            ("backspace", 8),
        ] {
            let stroke = Keystroke::parse(&format!("ctrl-{key}")).unwrap();
            assert_eq!(terminal_key_bytes(&stroke, TermMode::NONE), [byte]);
        }
        for (key, expected) in [
            ("shift-tab", "\x1b[Z"),
            ("ctrl-alt-up", "\x1b[1;7A"),
            ("alt-enter", "\x1b\r"),
            ("alt-a", "\x1ba"),
            ("cmd-a", ""),
            ("ctrl-f13", ""),
        ] {
            assert_eq!(
                terminal_key_bytes(&Keystroke::parse(key).unwrap(), TermMode::NONE),
                expected.as_bytes()
            );
        }
        let stroke = Keystroke {
            key: "é".into(),
            key_char: Some("é界".into()),
            ..Default::default()
        };
        assert_eq!(
            terminal_key_bytes(&stroke, TermMode::APP_KEYPAD),
            "é界".as_bytes()
        );
        let stroke = Keystroke {
            key: "b".into(),
            key_char: Some("∫".into()),
            modifiers: gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
        };
        assert_eq!(terminal_key_bytes(&stroke, TermMode::NONE), b"\x1bb");
        let stroke = Keystroke {
            key: "2".into(),
            key_char: Some("2".into()),
            ..Default::default()
        };
        assert_eq!(terminal_key_bytes(&stroke, TermMode::APP_KEYPAD), b"2");
    }
}
