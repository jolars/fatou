//! `fatou.toml` configuration: schema, defaults, and discovery.
//!
//! Defaults follow common Julia conventions (line width 92, 4-space indent).
//! Discovery walks up from an anchor directory looking for a `fatou.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::formatter::LineEnding;
use crate::linter::Severity;

pub const CONFIG_FILE_NAME: &str = "fatou.toml";

const DEFAULT_LINE_WIDTH: u32 = 92;
const DEFAULT_INDENT_WIDTH: u32 = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub format: FormatConfig,
    pub lint: LintConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatConfig {
    pub line_width: u32,
    pub indent_width: u32,
    /// The newline style the formatter emits. See [`LineEndingConfig`].
    pub line_ending: LineEnding,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LintConfig {
    /// If `Some`, only these rule IDs run; otherwise every default-on rule runs.
    pub select: Option<Vec<String>>,
    /// Rule IDs to disable.
    pub ignore: Vec<String>,
    /// Per-rule severity overrides (`[lint.severity]`); rules not listed keep
    /// their default severity.
    pub severity: BTreeMap<String, Severity>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            line_width: DEFAULT_LINE_WIDTH,
            indent_width: DEFAULT_INDENT_WIDTH,
            line_ending: LineEnding::default(),
        }
    }
}

/// The `line-ending` key under `[format]`. A thin, serde-named mirror of
/// [`LineEnding`] (the formatter's own type), kept separate so the TOML spelling
/// (`kebab-case`) is a config concern, not baked into the formatter API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LineEndingConfig {
    /// Detect per file from the source; default `\n` when none is present.
    #[default]
    Auto,
    /// Always `\n` (Unix).
    Lf,
    /// Always `\r\n` (Windows).
    Crlf,
    /// `\n` on Unix, `\r\n` on Windows.
    Native,
}

impl From<LineEndingConfig> for LineEnding {
    fn from(value: LineEndingConfig) -> Self {
        match value {
            LineEndingConfig::Auto => LineEnding::Auto,
            LineEndingConfig::Lf => LineEnding::Lf,
            LineEndingConfig::Crlf => LineEnding::Crlf,
            LineEndingConfig::Native => LineEnding::Native,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read { path: PathBuf, message: String },
    Parse { path: PathBuf, message: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            ConfigError::Parse { path, message } => {
                write!(f, "failed to parse {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// The on-disk TOML shape. Every field optional so a partial file falls back to
/// defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    format: RawFormat,
    #[serde(default)]
    lint: RawLint,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFormat {
    #[serde(rename = "line-width")]
    line_width: Option<u32>,
    #[serde(rename = "indent-width")]
    indent_width: Option<u32>,
    /// Deprecated snake_case alias for `line-width`, still accepted with a warning.
    #[serde(rename = "line_width")]
    line_width_snake: Option<u32>,
    /// Deprecated snake_case alias for `indent-width`, still accepted with a warning.
    #[serde(rename = "indent_width")]
    indent_width_snake: Option<u32>,
    #[serde(rename = "line-ending")]
    line_ending: Option<LineEndingConfig>,
}

impl RawFormat {
    /// Resolve to concrete widths, preferring the canonical kebab-case keys and
    /// recording a deprecation warning for any snake_case key that was present.
    fn resolve(self, defaults: &FormatConfig, warnings: &mut Vec<String>) -> FormatConfig {
        if self.line_width_snake.is_some() {
            warnings.push(deprecated_key("line_width", "line-width"));
        }
        if self.indent_width_snake.is_some() {
            warnings.push(deprecated_key("indent_width", "indent-width"));
        }
        FormatConfig {
            line_width: self
                .line_width
                .or(self.line_width_snake)
                .unwrap_or(defaults.line_width),
            indent_width: self
                .indent_width
                .or(self.indent_width_snake)
                .unwrap_or(defaults.indent_width),
            line_ending: self
                .line_ending
                .map(LineEnding::from)
                .unwrap_or(defaults.line_ending),
        }
    }
}

/// Message for a deprecated snake_case `[format]` key.
fn deprecated_key(old: &str, new: &str) -> String {
    format!("`{old}` in [format] is deprecated; use `{new}`")
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLint {
    select: Option<Vec<String>>,
    #[serde(default)]
    ignore: Vec<String>,
    #[serde(default)]
    severity: BTreeMap<String, Severity>,
}

impl Config {
    /// Resolve configuration. With `no_config`, return defaults. With an
    /// explicit path, load exactly that file. Otherwise discover the nearest
    /// `fatou.toml` walking up from `anchor`. Returns the config and the path it
    /// was loaded from (if any).
    ///
    /// Along with the config and its source path, returns any deprecation
    /// warnings raised while parsing (e.g. snake_case `[format]` keys).
    pub fn resolve(
        explicit: Option<&Path>,
        no_config: bool,
        anchor: &Path,
    ) -> Result<(Self, Option<PathBuf>, Vec<String>), ConfigError> {
        if no_config {
            return Ok((Self::default(), None, Vec::new()));
        }
        if let Some(path) = explicit {
            let (config, warnings) = Self::load(path)?;
            return Ok((config, Some(path.to_path_buf()), warnings));
        }
        match discover(anchor) {
            Some(path) => {
                let (config, warnings) = Self::load(&path)?;
                Ok((config, Some(path), warnings))
            }
            None => Ok((Self::default(), None, Vec::new())),
        }
    }

    fn load(path: &Path) -> Result<(Self, Vec<String>), ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|err| ConfigError::Read {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        let raw: RawConfig = toml::from_str(&text).map_err(|err| ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Ok(raw.into_config())
    }
}

impl RawConfig {
    fn into_config(self) -> (Config, Vec<String>) {
        let defaults = FormatConfig::default();
        let mut warnings = Vec::new();
        let config = Config {
            format: self.format.resolve(&defaults, &mut warnings),
            lint: LintConfig {
                select: self.lint.select,
                ignore: self.lint.ignore,
                severity: self.lint.severity,
            },
        };
        (config, warnings)
    }
}

/// Walk up from `anchor` looking for a `fatou.toml`.
fn discover(anchor: &Path) -> Option<PathBuf> {
    for dir in anchor.ancestors() {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_julia_conventions() {
        let config = Config::default();
        assert_eq!(config.format.line_width, 92);
        assert_eq!(config.format.indent_width, 4);
        assert_eq!(config.format.line_ending, LineEnding::Auto);
    }

    #[test]
    fn line_ending_defaults_to_auto() {
        let raw: RawConfig = toml::from_str("[format]\n").unwrap();
        let (config, _) = raw.into_config();
        assert_eq!(config.format.line_ending, LineEnding::Auto);
    }

    #[test]
    fn parses_line_ending_variants() {
        for (key, expected) in [
            ("auto", LineEnding::Auto),
            ("lf", LineEnding::Lf),
            ("crlf", LineEnding::Crlf),
            ("native", LineEnding::Native),
        ] {
            let text = format!("[format]\nline-ending = \"{key}\"\n");
            let raw: RawConfig = toml::from_str(&text).unwrap();
            let (config, _) = raw.into_config();
            assert_eq!(config.format.line_ending, expected, "for {key}");
        }
    }

    #[test]
    fn rejects_unknown_line_ending() {
        toml::from_str::<RawConfig>("[format]\nline-ending = \"mac\"\n")
            .expect_err("unknown variant should be rejected");
    }

    #[test]
    fn parses_partial_toml() {
        let raw: RawConfig = toml::from_str("[format]\nline-width = 100\n").unwrap();
        let (config, warnings) = raw.into_config();
        assert_eq!(config.format.line_width, 100);
        assert_eq!(config.format.indent_width, 4);
        assert!(warnings.is_empty());
    }

    #[test]
    fn snake_case_keys_are_accepted_with_a_warning() {
        let raw: RawConfig =
            toml::from_str("[format]\nline_width = 100\nindent_width = 2\n").unwrap();
        let (config, warnings) = raw.into_config();
        assert_eq!(config.format.line_width, 100);
        assert_eq!(config.format.indent_width, 2);
        assert_eq!(
            warnings,
            vec![
                "`line_width` in [format] is deprecated; use `line-width`".to_string(),
                "`indent_width` in [format] is deprecated; use `indent-width`".to_string(),
            ],
        );
    }

    #[test]
    fn kebab_case_wins_when_both_forms_present() {
        let raw: RawConfig =
            toml::from_str("[format]\nline-width = 100\nline_width = 80\n").unwrap();
        let (config, warnings) = raw.into_config();
        assert_eq!(config.format.line_width, 100);
        // The deprecated key is still reported even though it is overridden.
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn parses_lint_severity_table() {
        let raw: RawConfig = toml::from_str(
            "[lint.severity]\nunused-binding = \"error\"\nunused-import = \"hint\"\n",
        )
        .unwrap();
        let (config, _) = raw.into_config();
        assert_eq!(
            config.lint.severity.get("unused-binding"),
            Some(&Severity::Error)
        );
        assert_eq!(
            config.lint.severity.get("unused-import"),
            Some(&Severity::Hint)
        );
    }

    #[test]
    fn rejects_unknown_severity_value() {
        toml::from_str::<RawConfig>("[lint.severity]\nunused-binding = \"fatal\"\n")
            .expect_err("unknown severity should be rejected");
    }

    #[test]
    fn no_config_returns_defaults() {
        let (config, path, warnings) =
            Config::resolve(None, true, Path::new("/nonexistent")).unwrap();
        assert_eq!(config, Config::default());
        assert!(path.is_none());
        assert!(warnings.is_empty());
    }
}
