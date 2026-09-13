//! Configuration: file location, TOML parsing, defaults, and validation.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde::{Deserialize, Deserializer};

/// Path of the config file relative to the config home (e.g. `~/.config`).
const FILE_NAME: &str = "quickbar/config.toml";

/// An 8-bit-per-channel RGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parse `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("`{text}`: expected a `#rrggbb` or `#rrggbbaa` color");
        let hex = text.strip_prefix('#').ok_or_else(invalid)?;
        if hex.len() != 6 && hex.len() != 8 {
            return Err(invalid());
        }
        let byte = |at: usize| {
            u8::from_str_radix(&hex[at..at + 2], 16).map_err(|_| invalid())
        };
        Ok(Self {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: if hex.len() == 8 { byte(6)? } else { 0xff },
        })
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Where a status-bar module's content is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// A status-bar module backed by an external command.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Module {
    pub name: String,
    pub exec: String,
    /// Re-run the command every N seconds and use its last line of output.
    pub interval: Option<u64>,
    /// Keep the command running and update on every line it prints.
    pub stream: bool,
    pub align: Align,
    /// Defaults to `bar.foreground` when absent.
    pub color: Option<Rgba>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for Module {
    fn default() -> Self {
        Self {
            name: String::new(),
            exec: String::new(),
            interval: None,
            stream: false,
            align: Align::default(),
            color: None,
            unknown: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Border {
    pub width: u32,
    pub color: Rgba,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for Border {
    fn default() -> Self {
        Self {
            width: 6,
            color: Rgba::new(0x3b, 0x42, 0x52, 0xff),
            unknown: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Bar {
    pub height: u32,
    pub background: Rgba,
    /// Font family name; `None` selects the system monospace family.
    pub font: Option<String>,
    pub font_size: f32,
    pub foreground: Rgba,
    pub hover_delay_ms: u64,
    pub hide_delay_ms: u64,
    /// Height of the top and bottom edge surfaces below the border.
    pub content_max_height: u32,
    pub module: Vec<Module>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            height: 28,
            background: Rgba::new(0x1e, 0x1e, 0x2e, 0xee),
            font: None,
            font_size: 13.0,
            foreground: Rgba::new(0xd8, 0xde, 0xe9, 0xff),
            hover_delay_ms: 150,
            hide_delay_ms: 400,
            content_max_height: 400,
            module: Vec::new(),
            unknown: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub max_visible: usize,
    pub timeout_ms: u64,
    pub background: Rgba,
    pub text_color: Rgba,
    pub urgent_color: Rgba,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            max_visible: 3,
            timeout_ms: 5000,
            background: Rgba::new(0x1e, 0x1e, 0x2e, 0xee),
            text_color: Rgba::new(0xd8, 0xde, 0xe9, 0xff),
            urgent_color: Rgba::new(0xbf, 0x61, 0x6a, 0xff),
            unknown: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub border: Border,
    pub bar: Bar,
    pub notifications: Notifications,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Config {
    /// Reject configurations whose values are well-formed TOML but unusable.
    pub fn validate(&self) -> Result<(), String> {
        if self.bar.font_size <= 0.0 {
            return Err("bar.font_size: must be greater than 0".to_string());
        }
        for (index, module) in self.bar.module.iter().enumerate() {
            let at = |field: &str| format!("bar.module[{index}].{field}");
            if module.name.is_empty() {
                return Err(format!("{}: must not be empty", at("name")));
            }
            if module.exec.is_empty() {
                return Err(format!("{}: must not be empty", at("exec")));
            }
            if module.interval.is_none() && !module.stream {
                return Err(format!(
                    "bar.module[{index}] ({}): needs either `interval` or `stream = true`",
                    module.name
                ));
            }
            if module.interval == Some(0) {
                return Err(format!("{}: must be greater than 0", at("interval")));
            }
        }
        Ok(())
    }

    /// Fields that were present in the file but are not part of the config.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        collect_unknown(&mut warnings, "", &self.unknown);
        collect_unknown(&mut warnings, "border.", &self.border.unknown);
        collect_unknown(&mut warnings, "bar.", &self.bar.unknown);
        collect_unknown(&mut warnings, "notifications.", &self.notifications.unknown);
        for (index, module) in self.bar.module.iter().enumerate() {
            collect_unknown(&mut warnings, &format!("bar.module[{index}]."), &module.unknown);
        }
        warnings
    }
}

fn collect_unknown(out: &mut Vec<String>, prefix: &str, unknown: &BTreeMap<String, toml::Value>) {
    for key in unknown.keys() {
        out.push(format!("ignoring unknown config field `{prefix}{key}`"));
    }
}

/// The environment-resolved config path, or `None` when neither
/// `XDG_CONFIG_HOME` nor `HOME` is set.
pub fn config_path() -> Option<PathBuf> {
    config_path_from(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn config_path_from(config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |value: OsString| (!value.is_empty()).then_some(value);
    let base = non_empty(config_home?)
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(non_empty(home?)?).join(".config")))?;
    Some(base.join(FILE_NAME))
}

/// Load the config from the environment-resolved path.
///
/// Returns the config and any non-fatal warnings. A missing file is not an
/// error: every field falls back to its default.
pub fn load() -> Result<(Config, Vec<String>), String> {
    match config_path() {
        Some(path) => load_from(&path),
        None => Ok((Config::default(), Vec::new())),
    }
}

/// Load the config from an explicit path, for tests and for `reload`.
pub fn load_from(path: &Path) -> Result<(Config, Vec<String>), String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Config::default(), Vec::new()));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let config: Config =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    config
        .validate()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let warnings = config.warnings();
    Ok((config, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, String> {
        let config: Config = toml::from_str(text).map_err(|error| error.to_string())?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config = parse("").unwrap();
        assert_eq!(config.border.width, 6);
        assert_eq!(config.border.color, Rgba::new(0x3b, 0x42, 0x52, 0xff));
        assert_eq!(config.bar.height, 28);
        assert_eq!(config.bar.font_size, 13.0);
        assert_eq!(config.bar.font, None);
        assert_eq!(config.bar.hover_delay_ms, 150);
        assert_eq!(config.bar.hide_delay_ms, 400);
        assert_eq!(config.bar.content_max_height, 400);
        assert!(config.bar.module.is_empty());
        assert_eq!(config.notifications.max_visible, 3);
        assert_eq!(config.notifications.timeout_ms, 5000);
        assert_eq!(config.notifications.urgent_color, Rgba::new(0xbf, 0x61, 0x6a, 0xff));
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let config = parse("[border]\nwidth = 10\n").unwrap();
        assert_eq!(config.border.width, 10);
        assert_eq!(config.border.color, Border::default().color);
        assert_eq!(config.bar.height, 28);
    }

    #[test]
    fn colors_parse_with_and_without_alpha() {
        assert_eq!(Rgba::parse("#3b4252").unwrap(), Rgba::new(0x3b, 0x42, 0x52, 0xff));
        assert_eq!(Rgba::parse("#3b4252ff").unwrap(), Rgba::new(0x3b, 0x42, 0x52, 0xff));
        assert_eq!(Rgba::parse("#1e1e2eee").unwrap(), Rgba::new(0x1e, 0x1e, 0x2e, 0xee));
        assert_eq!(Rgba::parse("#00000000").unwrap(), Rgba::new(0, 0, 0, 0));
    }

    #[test]
    fn invalid_colors_are_rejected() {
        for bad in ["red", "#xyz", "#12345", "#1234567", "3b4252"] {
            let error = Rgba::parse(bad).unwrap_err();
            assert!(
                error.contains("expected a `#rrggbb`"),
                "unexpected message for {bad}: {error}"
            );
        }
    }

    #[test]
    fn invalid_color_reports_the_offending_field() {
        let error = parse("[border]\ncolor = \"red\"\n").unwrap_err();
        assert!(error.contains("red"), "message should quote the value: {error}");
    }

    #[test]
    fn zero_font_size_is_rejected() {
        let error = parse("[bar]\nfont_size = 0\n").unwrap_err();
        assert!(error.contains("bar.font_size"), "unexpected message: {error}");
    }

    #[test]
    fn module_needs_interval_or_stream() {
        let error = parse("[[bar.module]]\nname = \"clock\"\nexec = \"date\"\n").unwrap_err();
        assert!(error.contains("interval"), "unexpected message: {error}");

        assert!(parse("[[bar.module]]\nname = \"clock\"\nexec = \"date\"\ninterval = 1\n").is_ok());
        assert!(parse("[[bar.module]]\nname = \"clock\"\nexec = \"date\"\nstream = true\n").is_ok());
    }

    #[test]
    fn module_zero_interval_is_rejected() {
        let error = parse("[[bar.module]]\nname = \"c\"\nexec = \"date\"\ninterval = 0\n").unwrap_err();
        assert!(error.contains("must be greater than 0"), "unexpected message: {error}");
    }

    #[test]
    fn module_defaults_and_alignment() {
        let config = parse(
            "[bar]\nforeground = \"#aabbcc\"\n\n[[bar.module]]\nname = \"clock\"\nexec = \"date\"\ninterval = 1\nalign = \"right\"\n",
        )
        .unwrap();
        let module = &config.bar.module[0];
        assert_eq!(module.align, Align::Right);
        assert_eq!(module.color, None);
        assert!(!module.stream);
    }

    #[test]
    fn unknown_fields_warn_but_load() {
        let config = parse("[border]\nweight = 3\n\n[notifications]\npanels = 2\n").unwrap();
        let warnings = config.warnings();
        assert_eq!(config.border.width, 6);
        assert!(warnings.iter().any(|w| w.contains("border.weight")), "{warnings:?}");
        assert!(
            warnings.iter().any(|w| w.contains("notifications.panels")),
            "{warnings:?}"
        );
    }

    #[test]
    fn unknown_module_fields_warn_too() {
        let config =
            parse("[[bar.module]]\nname = \"c\"\nexec = \"date\"\ninterval = 1\ntooltip = \"x\"\n")
                .unwrap();
        assert_eq!(config.warnings(), vec!["ignoring unknown config field `bar.module[0].tooltip`"]);
    }

    #[test]
    fn config_path_prefers_xdg_config_home() {
        let from_xdg = config_path_from(Some("/xdg".into()), Some("/home/u".into()));
        assert_eq!(from_xdg, Some(PathBuf::from("/xdg/quickbar/config.toml")));

        let from_home = config_path_from(Some(OsString::new()), Some("/home/u".into()));
        assert_eq!(from_home, Some(PathBuf::from("/home/u/.config/quickbar/config.toml")));

        assert_eq!(config_path_from(None, None), None);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = env::temp_dir().join(format!("quickbar-absent-{}.toml", std::process::id()));
        let (config, warnings) = load_from(&path).unwrap();
        assert_eq!(config.border.width, 6);
        assert!(warnings.is_empty());
    }

    #[test]
    fn loaded_file_reports_parse_errors_with_the_path() {
        let path = env::temp_dir().join(format!("quickbar-bad-{}.toml", std::process::id()));
        fs::write(&path, "[border]\ncolor = \"nope\"\n").unwrap();
        let error = load_from(&path).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert!(error.contains("quickbar-bad-"), "should name the file: {error}");
    }
}
