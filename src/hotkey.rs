use std::{fmt, str::FromStr};

pub const DEFAULT_VISIBILITY_HOTKEY: &str = "Ctrl+Alt+H";

/// A Windows virtual key and RegisterHotKey modifier mask (without MOD_NOREPEAT).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

const NAMED_KEYS: &[(u32, &str)] = &[
    (0x08, "Backspace"),
    (0x09, "Tab"),
    (0x0D, "Enter"),
    (0x13, "Pause"),
    (0x14, "CapsLock"),
    (0x1B, "Escape"),
    (0x20, "Space"),
    (0x21, "PageUp"),
    (0x22, "PageDown"),
    (0x23, "End"),
    (0x24, "Home"),
    (0x25, "Left"),
    (0x26, "Up"),
    (0x27, "Right"),
    (0x28, "Down"),
    (0x2D, "Insert"),
    (0x2E, "Delete"),
    (0x6A, "Multiply"),
    (0x6B, "Add"),
    (0x6D, "Subtract"),
    (0x6E, "Decimal"),
    (0x6F, "Divide"),
    (0x90, "NumLock"),
    (0x91, "ScrollLock"),
    (0xBA, "Semicolon"),
    (0xBB, "Equals"),
    (0xBC, "Comma"),
    (0xBD, "Minus"),
    (0xBE, "Period"),
    (0xBF, "Slash"),
    (0xC0, "Backtick"),
    (0xDB, "LeftBracket"),
    (0xDC, "Backslash"),
    (0xDD, "RightBracket"),
    (0xDE, "Quote"),
];

impl Hotkey {
    pub fn from_virtual_key(modifiers: u32, key: u32) -> Result<Self, String> {
        if key == 0x7B {
            return Err("F12 是 Windows 保留键，请换一个快捷键".into());
        }
        if !matches!(key, 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x69 | 0x70..=0x87)
            && !NAMED_KEYS.iter().any(|(code, _)| *code == key)
        {
            return Err("暂不支持这个按键，请按字母、数字、功能键或常用控制键".into());
        }
        Ok(Self {
            modifiers: modifiers & 0xF,
            key,
        })
    }

    pub fn is_modifier_key(key: u32) -> bool {
        matches!(key, 0x10..=0x12 | 0x5B..=0x5C | 0xA0..=0xA5)
    }
}

impl FromStr for Hotkey {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut modifiers = 0;
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            let lower = part.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "ctrl" | "control" => Some(0x0002),
                "alt" => Some(0x0001),
                "shift" => Some(0x0004),
                "win" | "windows" | "super" => Some(0x0008),
                _ => None,
            };
            if let Some(modifier) = modifier {
                if modifiers & modifier != 0 {
                    return Err(format!("修饰键 {part} 重复了"));
                }
                modifiers |= modifier;
                continue;
            }
            if part.is_empty() {
                return Err("请输入快捷键，例如 Ctrl+Alt+H 或 F8".into());
            }
            if key.is_some() {
                return Err("只能使用一个主键，可搭配多个 Ctrl / Alt / Shift / Win 修饰键".into());
            }
            key = if lower.len() == 1 && lower.as_bytes()[0].is_ascii_alphanumeric() {
                Some(lower.as_bytes()[0].to_ascii_uppercase() as u32)
            } else if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
            {
                (1..=24).contains(&number).then(|| 0x70 + number - 1)
            } else if let Some(number) = lower
                .strip_prefix("numpad")
                .and_then(|n| n.parse::<u32>().ok())
            {
                (number <= 9).then_some(0x60 + number.min(9))
            } else {
                let name = match lower.as_str() {
                    "esc" => "escape",
                    "return" => "enter",
                    "pgup" => "pageup",
                    "pgdn" => "pagedown",
                    "del" => "delete",
                    "ins" => "insert",
                    _ => lower.as_str(),
                };
                NAMED_KEYS
                    .iter()
                    .find(|(_, label)| label.eq_ignore_ascii_case(name))
                    .map(|(key, _)| *key)
            };
            if key.is_none() {
                return Err(format!(
                    "不支持按键“{part}”，请使用字母、数字、F1–F24 或 Space 等常用键"
                ));
            }
        }
        let key = key.ok_or("还需要一个主键，例如 Ctrl+Alt+H 中的 H")?;
        Self::from_virtual_key(modifiers, key)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (mask, name) in [(2, "Ctrl"), (1, "Alt"), (4, "Shift"), (8, "Win")] {
            if self.modifiers & mask != 0 {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            0x30..=0x39 | 0x41..=0x5A => write!(f, "{}", char::from(self.key as u8)),
            0x60..=0x69 => write!(f, "NumPad{}", self.key - 0x60),
            0x70..=0x87 => write!(f, "F{}", self.key - 0x70 + 1),
            key => f.write_str(
                NAMED_KEYS
                    .iter()
                    .find(|(code, _)| *code == key)
                    .map_or("?", |(_, name)| name),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_function_keys_and_all_modifier_combinations_roundtrip() {
        for key in 0x70..=0x77 {
            for modifiers in 0..=15 {
                let binding = Hotkey::from_virtual_key(modifiers, key).unwrap();
                assert_eq!(binding.key, key);
                assert_eq!(binding.modifiers, modifiers);
                assert_eq!(binding.to_string().parse::<Hotkey>().unwrap(), binding);
            }
        }
        assert_eq!(Hotkey::from_virtual_key(0, 0x70).unwrap().to_string(), "F1");
        assert_eq!(
            Hotkey::from_virtual_key(6, 0x77).unwrap().to_string(),
            "Ctrl+Shift+F8"
        );
        for &(key, _) in NAMED_KEYS {
            let binding = Hotkey::from_virtual_key(3, key).unwrap();
            assert_eq!(binding.to_string().parse::<Hotkey>().unwrap(), binding);
        }
        for key in [
            0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
        ] {
            assert!(Hotkey::is_modifier_key(key));
            assert!(Hotkey::from_virtual_key(0, key).is_err());
        }
        assert!(Hotkey::from_virtual_key(0, 0x7B).is_err());
    }

    #[test]
    fn accepts_and_normalizes_single_keys_and_modifier_combinations() {
        for (input, canonical, modifiers, key) in [
            (" alt + Control + h ", "Ctrl+Alt+H", 3, 0x48),
            ("Shift+win+CTRL+alt+9", "Ctrl+Alt+Shift+Win+9", 15, 0x39),
            ("f8", "F8", 0, 0x77),
            ("F24", "F24", 0, 0x87),
            ("Ctrl+Space", "Ctrl+Space", 2, 0x20),
            ("Shift+PgDn", "Shift+PageDown", 4, 0x22),
            ("Alt+NumPad0", "Alt+NumPad0", 1, 0x60),
            ("a", "A", 0, 0x41),
        ] {
            let hotkey: Hotkey = input.parse().unwrap();
            assert_eq!(hotkey, Hotkey { modifiers, key });
            assert_eq!(hotkey.to_string(), canonical);
            assert_eq!(canonical.parse::<Hotkey>().unwrap(), hotkey);
        }
    }

    #[test]
    fn rejects_incomplete_multiple_and_reserved_keys_without_panicking() {
        for input in [
            "",
            "Ctrl",
            "Ctrl+Alt",
            "Ctrl+",
            "Ctrl++H",
            "Ctrl+H+J",
            "Ctrl+Ctrl+H",
            "F0",
            "F25",
            "F12",
            "Ctrl+F12",
            "NumPad10",
            "Ctrl+鼠标",
            "F4294967295",
            "NumPad4294967295",
        ] {
            assert!(input.parse::<Hotkey>().is_err(), "{input}");
        }
    }
}
