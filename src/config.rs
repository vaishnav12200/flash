use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::font::{DEFAULT_FONT_PATH, DEFAULT_FONT_SIZE};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub scrollback: ScrollbackConfig,
    pub keybindings: KeybindingsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub path: PathBuf,
    pub fallback: Vec<PathBuf>,
    pub size: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from(DEFAULT_FONT_PATH),
            fallback: Vec::new(),
            size: DEFAULT_FONT_SIZE,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub padding_x: f32,
    pub padding_y: f32,
    pub foreground: String,
    pub background: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            padding_x: 8.0,
            padding_y: 8.0,
            foreground: "#E6EBF5".to_owned(),
            background: "#090A0E".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ScrollbackConfig {
    pub lines: usize,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self { lines: 10_000 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub copy: String,
    pub paste: String,
    pub increase_font: String,
    pub decrease_font: String,
    pub reset_font: String,
    pub scroll_page_up: String,
    pub scroll_page_down: String,
    pub scroll_to_bottom: String,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            copy: "Ctrl+Shift+C".to_owned(),
            paste: "Ctrl+Shift+V".to_owned(),
            increase_font: "Ctrl+Shift+Plus".to_owned(),
            decrease_font: "Ctrl+Minus".to_owned(),
            reset_font: "Ctrl+0".to_owned(),
            scroll_page_up: "Shift+PageUp".to_owned(),
            scroll_page_down: "Shift+PageDown".to_owned(),
            scroll_to_bottom: "Ctrl+Shift+End".to_owned(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("could not parse {}", path.display()))?;
        config
            .validate()
            .with_context(|| format!("invalid configuration in {}", path.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if !self.font.size.is_finite() || !(6.0..=72.0).contains(&self.font.size) {
            bail!("font.size must be between 6 and 72");
        }
        if self.font.path.as_os_str().is_empty() {
            bail!("font.path must not be empty");
        }
        if self
            .font
            .fallback
            .iter()
            .any(|path| path.as_os_str().is_empty())
        {
            bail!("font.fallback paths must not be empty");
        }
        if !self.window.padding_x.is_finite() || self.window.padding_x < 0.0 {
            bail!("window.padding_x must be non-negative");
        }
        if !self.window.padding_y.is_finite() || self.window.padding_y < 0.0 {
            bail!("window.padding_y must be non-negative");
        }
        if self.scrollback.lines > 1_000_000 {
            bail!("scrollback.lines must not exceed 1000000");
        }
        parse_color(&self.window.foreground).context("window.foreground")?;
        parse_color(&self.window.background).context("window.background")?;
        ShortcutMap::from_config(&self.keybindings)?;
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        return PathBuf::from(path).join("flash/config.toml");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/flash/config.toml")
}

pub fn parse_color(value: &str) -> Result<[f32; 4]> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        bail!("expected #RRGGBB");
    }
    let red = u8::from_str_radix(&value[0..2], 16).context("invalid red component")?;
    let green = u8::from_str_radix(&value[2..4], 16).context("invalid green component")?;
    let blue = u8::from_str_radix(&value[4..6], 16).context("invalid blue component")?;
    Ok([
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        1.0,
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    Copy,
    Paste,
    IncreaseFont,
    DecreaseFont,
    ResetFont,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToBottom,
}

pub struct ShortcutMap(Vec<(Shortcut, ShortcutAction)>);

impl ShortcutMap {
    pub fn from_config(config: &KeybindingsConfig) -> Result<Self> {
        let bindings = [
            (&config.copy, ShortcutAction::Copy),
            (&config.paste, ShortcutAction::Paste),
            (&config.increase_font, ShortcutAction::IncreaseFont),
            (&config.decrease_font, ShortcutAction::DecreaseFont),
            (&config.reset_font, ShortcutAction::ResetFont),
            (&config.scroll_page_up, ShortcutAction::ScrollPageUp),
            (&config.scroll_page_down, ShortcutAction::ScrollPageDown),
            (&config.scroll_to_bottom, ShortcutAction::ScrollToBottom),
        ];
        let mut shortcuts = Vec::with_capacity(bindings.len());
        for (binding, action) in bindings {
            shortcuts.push((
                Shortcut::parse(binding)
                    .with_context(|| format!("invalid shortcut {binding:?}"))?,
                action,
            ));
        }
        Ok(Self(shortcuts))
    }

    pub fn action(&self, key: &Key, modifiers: ModifiersState) -> Option<ShortcutAction> {
        self.0
            .iter()
            .find_map(|(shortcut, action)| shortcut.matches(key, modifiers).then_some(*action))
    }
}

struct Shortcut {
    control: bool,
    alt: bool,
    shift: bool,
    super_key: bool,
    key: ShortcutKey,
}

enum ShortcutKey {
    Character(String),
    Named(NamedKey),
}

impl Shortcut {
    fn parse(value: &str) -> Result<Self> {
        let mut shortcut = Self {
            control: false,
            alt: false,
            shift: false,
            super_key: false,
            key: ShortcutKey::Character(String::new()),
        };
        let mut key = None;
        for part in value
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => shortcut.control = true,
                "alt" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                "super" | "logo" => shortcut.super_key = true,
                _ if key.is_none() => key = Some(parse_shortcut_key(part)?),
                _ => bail!("shortcut contains multiple keys"),
            }
        }
        shortcut.key = key.context("shortcut has no key")?;
        Ok(shortcut)
    }

    fn matches(&self, key: &Key, modifiers: ModifiersState) -> bool {
        if self.control != modifiers.control_key()
            || self.alt != modifiers.alt_key()
            || self.shift != modifiers.shift_key()
            || self.super_key != modifiers.super_key()
        {
            return false;
        }
        match (&self.key, key) {
            (ShortcutKey::Character(expected), Key::Character(actual)) => {
                actual.eq_ignore_ascii_case(expected)
            }
            (ShortcutKey::Named(expected), Key::Named(actual)) => expected == actual,
            _ => false,
        }
    }
}

fn parse_shortcut_key(value: &str) -> Result<ShortcutKey> {
    let named = match value.to_ascii_lowercase().as_str() {
        "pageup" => Some(NamedKey::PageUp),
        "pagedown" => Some(NamedKey::PageDown),
        "home" => Some(NamedKey::Home),
        "end" => Some(NamedKey::End),
        "insert" => Some(NamedKey::Insert),
        "delete" => Some(NamedKey::Delete),
        "plus" => return Ok(ShortcutKey::Character("+".to_owned())),
        "minus" => return Ok(ShortcutKey::Character("-".to_owned())),
        _ => None,
    };
    if let Some(named) = named {
        return Ok(ShortcutKey::Named(named));
    }
    if value.chars().count() == 1 {
        return Ok(ShortcutKey::Character(value.to_owned()));
    }
    bail!("unknown key {value:?}")
}

#[cfg(test)]
mod tests {
    use super::{Config, KeybindingsConfig, ShortcutAction, ShortcutMap, parse_color};
    use winit::keyboard::{Key, ModifiersState};

    #[test]
    fn parses_hex_colors() {
        assert_eq!(
            parse_color("#FF8000").unwrap(),
            [1.0, 128.0 / 255.0, 0.0, 1.0]
        );
        assert!(parse_color("orange").is_err());
    }

    #[test]
    fn partial_toml_uses_defaults_for_unspecified_fields() {
        let config: Config = toml::from_str(
            r##"
                [font]
                size = 20.0

                [window]
                background = "#112233"
            "##,
        )
        .unwrap();
        assert_eq!(config.font.size, 20.0);
        assert_eq!(config.window.background, "#112233");
        assert_eq!(config.scrollback.lines, 10_000);
        assert_eq!(config.keybindings.copy, "Ctrl+Shift+C");
    }

    #[test]
    fn parses_ordered_fallback_font_paths() {
        let config: Config = toml::from_str(
            r#"
                [font]
                fallback = ["/fonts/cjk.ttf", "/fonts/symbols.otf"]
            "#,
        )
        .unwrap();
        assert_eq!(config.font.fallback.len(), 2);
        assert_eq!(
            config.font.fallback[0],
            std::path::Path::new("/fonts/cjk.ttf")
        );
    }

    #[test]
    fn configurable_copy_does_not_match_plain_ctrl_c() {
        let shortcuts = ShortcutMap::from_config(&KeybindingsConfig::default()).unwrap();
        let ctrl = ModifiersState::CONTROL;
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(shortcuts.action(&Key::Character("c".into()), ctrl), None);
        assert_eq!(
            shortcuts.action(&Key::Character("c".into()), ctrl_shift),
            Some(ShortcutAction::Copy)
        );
    }
}
