//! `fatou.toml` configuration: schema, defaults, and discovery.
//!
//! Defaults follow common Julia conventions (line width 92, 4-space indent).
//! Discovery walks up from an anchor directory looking for a `fatou.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LintConfig {
    /// If `Some`, only these rule IDs run; otherwise every default-on rule runs.
    pub select: Option<Vec<String>>,
    /// Rule IDs to disable.
    pub ignore: Vec<String>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            line_width: DEFAULT_LINE_WIDTH,
            indent_width: DEFAULT_INDENT_WIDTH,
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
    line_width: Option<u32>,
    indent_width: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLint {
    select: Option<Vec<String>>,
    #[serde(default)]
    ignore: Vec<String>,
}

impl Config {
    /// Resolve configuration. With `no_config`, return defaults. With an
    /// explicit path, load exactly that file. Otherwise discover the nearest
    /// `fatou.toml` walking up from `anchor`. Returns the config and the path it
    /// was loaded from (if any).
    pub fn resolve(
        explicit: Option<&Path>,
        no_config: bool,
        anchor: &Path,
    ) -> Result<(Self, Option<PathBuf>), ConfigError> {
        if no_config {
            return Ok((Self::default(), None));
        }
        if let Some(path) = explicit {
            return Ok((Self::load(path)?, Some(path.to_path_buf())));
        }
        match discover(anchor) {
            Some(path) => {
                let config = Self::load(&path)?;
                Ok((config, Some(path)))
            }
            None => Ok((Self::default(), None)),
        }
    }

    fn load(path: &Path) -> Result<Self, ConfigError> {
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
    fn into_config(self) -> Config {
        let defaults = FormatConfig::default();
        Config {
            format: FormatConfig {
                line_width: self.format.line_width.unwrap_or(defaults.line_width),
                indent_width: self.format.indent_width.unwrap_or(defaults.indent_width),
            },
            lint: LintConfig {
                select: self.lint.select,
                ignore: self.lint.ignore,
            },
        }
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
    }

    #[test]
    fn parses_partial_toml() {
        let raw: RawConfig = toml::from_str("[format]\nline_width = 100\n").unwrap();
        let config = raw.into_config();
        assert_eq!(config.format.line_width, 100);
        assert_eq!(config.format.indent_width, 4);
    }

    #[test]
    fn no_config_returns_defaults() {
        let (config, path) = Config::resolve(None, true, Path::new("/nonexistent")).unwrap();
        assert_eq!(config, Config::default());
        assert!(path.is_none());
    }
}
