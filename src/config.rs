use std::{env, fs, path::PathBuf, sync::OnceLock};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::font::{DEFAULT_FONT_PATH, DEFAULT_FONT_SIZE};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub colors: ColorsConfig,
    pub cursor: CursorConfig,
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
    /// Deprecated compatibility aliases. When present, these override the
    /// corresponding `[colors]` values.
    pub foreground: Option<String>,
    pub background: Option<String>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            padding_x: 20.0,
            padding_y: 16.0,
            foreground: None,
            background: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub accent: String,
    pub selection_background: String,
    pub selection_foreground: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            background: "#080A0D".to_owned(),
            foreground: "#D8DEE9".to_owned(),
            cursor: "#41E66B".to_owned(),
            accent: "#FF8A2A".to_owned(),
            selection_background: "#3A261E".to_owned(),
            selection_foreground: "#F2E9E1".to_owned(),
            black: "#161A1F".to_owned(),
            red: "#D96666".to_owned(),
            green: "#72C991".to_owned(),
            yellow: "#D99A5E".to_owned(),
            blue: "#6A9FD0".to_owned(),
            magenta: "#B27AB4".to_owned(),
            cyan: "#58B8B0".to_owned(),
            white: "#C5CBD3".to_owned(),
            bright_black: "#606873".to_owned(),
            bright_red: "#E27772".to_owned(),
            bright_green: "#8AD5A5".to_owned(),
            bright_yellow: "#E8BB6A".to_owned(),
            bright_blue: "#80B1DF".to_owned(),
            bright_magenta: "#C48BC5".to_owned(),
            bright_cyan: "#70CEC2".to_owned(),
            bright_white: "#F0F2F5".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorConfig {
    pub style: CursorStyle,
    pub blink: bool,
    pub blink_interval: u64,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            style: CursorStyle::Block,
            blink: true,
            blink_interval: 600,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VisualColors {
    pub background: [f32; 4],
    pub foreground: [f32; 4],
    pub cursor: [f32; 4],
    pub selection_background: [f32; 4],
    pub selection_foreground: [f32; 4],
    pub ansi: [[f32; 4]; 16],
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
        if !(100..=2_000).contains(&self.cursor.blink_interval) {
            bail!("cursor.blink_interval must be between 100 and 2000 milliseconds");
        }
        self.visual_colors()?;
        ShortcutMap::from_config(&self.keybindings)?;
        Ok(())
    }

    pub fn visual_colors(&self) -> Result<VisualColors> {
        let colors = &self.colors;
        let foreground = self
            .window
            .foreground
            .as_deref()
            .unwrap_or(&colors.foreground);
        let background = self
            .window
            .background
            .as_deref()
            .unwrap_or(&colors.background);
        let named = [
            ("colors.black", &colors.black),
            ("colors.red", &colors.red),
            ("colors.green", &colors.green),
            ("colors.yellow", &colors.yellow),
            ("colors.blue", &colors.blue),
            ("colors.magenta", &colors.magenta),
            ("colors.cyan", &colors.cyan),
            ("colors.white", &colors.white),
            ("colors.bright_black", &colors.bright_black),
            ("colors.bright_red", &colors.bright_red),
            ("colors.bright_green", &colors.bright_green),
            ("colors.bright_yellow", &colors.bright_yellow),
            ("colors.bright_blue", &colors.bright_blue),
            ("colors.bright_magenta", &colors.bright_magenta),
            ("colors.bright_cyan", &colors.bright_cyan),
            ("colors.bright_white", &colors.bright_white),
        ];
        let mut ansi = [[0.0; 4]; 16];
        for (index, (name, value)) in named.into_iter().enumerate() {
            ansi[index] = parse_color(value).with_context(|| name.to_owned())?;
        }
        parse_color(&colors.accent).context("colors.accent")?;
        Ok(VisualColors {
            background: parse_color(background).context(if self.window.background.is_some() {
                "window.background"
            } else {
                "colors.background"
            })?,
            foreground: parse_color(foreground).context(if self.window.foreground.is_some() {
                "window.foreground"
            } else {
                "colors.foreground"
            })?,
            cursor: parse_color(&colors.cursor).context("colors.cursor")?,
            selection_background: parse_color(&colors.selection_background)
                .context("colors.selection_background")?,
            selection_foreground: parse_color(&colors.selection_foreground)
                .context("colors.selection_foreground")?,
            ansi,
        })
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
        srgb_to_linear(red),
        srgb_to_linear(green),
        srgb_to_linear(blue),
        1.0,
    ])
}

pub(crate) fn srgb_to_linear(component: u8) -> f32 {
    static LOOKUP: OnceLock<[f32; 256]> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        std::array::from_fn(|index| {
            let value = index as f32 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })[component as usize]
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
    use super::{Config, CursorStyle, KeybindingsConfig, ShortcutAction, ShortcutMap, parse_color};
    use winit::keyboard::{Key, ModifiersState};

    #[test]
    fn parses_hex_colors() {
        let color = parse_color("#FF8000").unwrap();
        assert_eq!(color[0], 1.0);
        assert!((color[1] - 0.215_860_53).abs() < 0.000_001);
        assert_eq!(color[2..], [0.0, 1.0]);
        assert!(parse_color("orange").is_err());
    }

    #[test]
    fn default_visual_system_uses_flash_palette_and_spacing() {
        let config = Config::default();
        assert_eq!(config.window.padding_x, 20.0);
        assert_eq!(config.window.padding_y, 16.0);
        assert_eq!(config.colors.background, "#080A0D");
        assert_eq!(config.colors.foreground, "#D8DEE9");
        assert_eq!(config.colors.cursor, "#41E66B");
        assert_eq!(config.colors.accent, "#FF8A2A");
        assert_eq!(config.cursor.style, CursorStyle::Block);
        assert!(config.cursor.blink);
        assert_eq!(config.cursor.blink_interval, 600);
        assert_eq!(config.visual_colors().unwrap().ansi.len(), 16);
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
        assert_eq!(config.window.background.as_deref(), Some("#112233"));
        assert_eq!(config.scrollback.lines, 10_000);
        assert_eq!(config.keybindings.copy, "Ctrl+Shift+C");
    }

    #[test]
    fn colors_section_and_cursor_styles_are_configurable() {
        let config: Config = toml::from_str(
            r##"
                [colors]
                background = "#101820"
                cyan = "#40C0D0"

                [cursor]
                style = "beam"
                blink = false
                blink_interval = 900
            "##,
        )
        .unwrap();
        assert_eq!(config.colors.background, "#101820");
        assert_eq!(config.colors.cyan, "#40C0D0");
        assert_eq!(config.cursor.style, CursorStyle::Beam);
        assert!(!config.cursor.blink);
        assert_eq!(config.cursor.blink_interval, 900);
        config.validate().unwrap();
    }

    #[test]
    fn rejects_cursor_blink_intervals_that_could_cause_busy_redraws() {
        let config: Config = toml::from_str(
            r#"
                [cursor]
                blink_interval = 50
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn legacy_window_colors_override_the_new_palette_fields() {
        let config: Config = toml::from_str(
            r##"
                [window]
                foreground = "#102030"
                background = "#405060"
            "##,
        )
        .unwrap();
        let visual = config.visual_colors().unwrap();
        assert_eq!(visual.foreground, parse_color("#102030").unwrap());
        assert_eq!(visual.background, parse_color("#405060").unwrap());
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
