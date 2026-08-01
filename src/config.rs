//! `fatou.toml` configuration: schema, defaults, and discovery.
//!
//! Defaults follow common Julia conventions (line width 92, 4-space indent).
//! Discovery walks up from an anchor directory looking for a `fatou.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::file_discovery::{ExcludeError, ExcludeFilter};
use crate::formatter::LineEnding;
use crate::julia_version::{VersionRange, parse_compat};
use crate::linter::Severity;

pub const CONFIG_FILE_NAME: &str = "fatou.toml";

const DEFAULT_LINE_WIDTH: u32 = 92;
const DEFAULT_INDENT_WIDTH: u32 = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub format: FormatConfig,
    pub lint: LintConfig,
    /// Julia-language settings (target version, and room to grow into
    /// environment-resolution overrides).
    pub julia: JuliaConfig,
    /// Gitignore-style patterns to exclude from file discovery, resolved
    /// relative to the directory containing `fatou.toml`.
    pub exclude: Vec<String>,
    /// Gitignore-style patterns to exclude *in addition to*
    /// [`exclude`](Self::exclude). Kept separate for forward compatibility: if
    /// `exclude` ever gains built-in defaults, setting it replaces them, while
    /// `extend-exclude` only ever adds patterns.
    pub extend_exclude: Vec<String>,
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

/// The `[julia]` section: settings tied to the Julia language and environment.
/// Currently just the target version; reserved as the home for future
/// environment-resolution overrides (project path, depots).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JuliaConfig {
    /// The declared target version or support range, used by the
    /// `julia-version-compat` rule. `None` leaves the version explicit-only,
    /// falling back to `Project.toml`/`Manifest.toml` discovery at the call site.
    pub version: Option<VersionRange>,
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
/// defaults. The serde derives are format-agnostic, so the LSP reuses this
/// shape to parse editor-pushed JSON settings (`initializationOptions`,
/// `workspace/didChangeConfiguration`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    #[serde(default)]
    format: RawFormat,
    #[serde(default)]
    lint: RawLint,
    #[serde(default)]
    julia: RawJulia,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(rename = "extend-exclude", default)]
    extend_exclude: Vec<String>,
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJulia {
    version: Option<String>,
}

impl RawJulia {
    /// Resolve the target version, recording a warning (and leaving it unset)
    /// when the spelling does not parse rather than failing the whole run.
    fn resolve(self, warnings: &mut Vec<String>) -> JuliaConfig {
        let version = self.version.and_then(|spec| match parse_compat(&spec) {
            Ok(range) => Some(range),
            Err(_) => {
                warnings.push(format!(
                    "`version` in [julia] is not a valid Julia version: `{spec}`"
                ));
                None
            }
        });
        JuliaConfig { version }
    }
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

    /// Build the file-discovery [`ExcludeFilter`] from this config's `exclude`
    /// and `extend-exclude` plus any `extra` patterns (e.g. CLI `--exclude`).
    /// Patterns are rooted at the directory containing the loaded config file
    /// (`config_path`), falling back to `anchor` when no config was found.
    pub fn exclude_filter(
        &self,
        config_path: Option<&Path>,
        anchor: &Path,
        extra: &[String],
    ) -> Result<ExcludeFilter, ExcludeError> {
        let root = config_path.and_then(Path::parent).unwrap_or(anchor);
        let mut patterns = self.exclude.clone();
        patterns.extend(self.extend_exclude.iter().cloned());
        patterns.extend(extra.iter().cloned());
        ExcludeFilter::new(root, &patterns)
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
    pub(crate) fn into_config(self) -> (Config, Vec<String>) {
        let defaults = FormatConfig::default();
        let mut warnings = Vec::new();
        let config = Config {
            format: self.format.resolve(&defaults, &mut warnings),
            lint: LintConfig {
                select: self.lint.select,
                ignore: self.lint.ignore,
                severity: self.lint.severity,
            },
            julia: self.julia.resolve(&mut warnings),
            exclude: self.exclude,
            extend_exclude: self.extend_exclude,
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
    fn parses_julia_version() {
        let raw: RawConfig = toml::from_str("[julia]\nversion = \"1.6\"\n").unwrap();
        let (config, warnings) = raw.into_config();
        let range = config.julia.version.expect("version parsed");
        assert_eq!(range.min, crate::julia_version::Version::new(1, 6, 0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn julia_defaults_to_no_version() {
        let (config, _) = RawConfig::default().into_config();
        assert_eq!(config.julia.version, None);
    }

    #[test]
    fn invalid_julia_version_warns_and_stays_unset() {
        let raw: RawConfig = toml::from_str("[julia]\nversion = \"nope\"\n").unwrap();
        let (config, warnings) = raw.into_config();
        assert_eq!(config.julia.version, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("[julia]"));
    }

    #[test]
    fn rejects_unknown_julia_key() {
        toml::from_str::<RawConfig>("[julia]\ntarget = \"1.6\"\n")
            .expect_err("unknown [julia] key should be rejected");
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
    fn parses_top_level_exclude_and_extend_exclude() {
        let raw: RawConfig =
            toml::from_str("exclude = [\"vendor/\"]\nextend-exclude = [\"generated/\"]\n").unwrap();
        let (config, warnings) = raw.into_config();
        assert_eq!(config.exclude, vec!["vendor/".to_string()]);
        assert_eq!(config.extend_exclude, vec!["generated/".to_string()]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn exclude_defaults_to_empty() {
        let config = Config::default();
        assert!(config.exclude.is_empty());
        assert!(config.extend_exclude.is_empty());
    }

    #[test]
    fn exclude_filter_combines_config_and_extra_patterns() {
        let config = Config {
            exclude: vec!["vendor/".to_string()],
            extend_exclude: vec!["generated/".to_string()],
            ..Config::default()
        };
        let filter = config
            .exclude_filter(None, Path::new("/tmp"), &["cli/".to_string()])
            .unwrap()
            .with_force_exclude(true);
        for dir in ["vendor", "generated", "cli"] {
            assert!(
                filter.force_excludes(Path::new(&format!("/tmp/{dir}/a.jl"))),
                "{dir} should be excluded"
            );
        }
        assert!(!filter.force_excludes(Path::new("/tmp/src/a.jl")));
    }

    #[test]
    fn exclude_filter_roots_at_config_file_directory() {
        let config = Config {
            exclude: vec!["vendor/".to_string()],
            ..Config::default()
        };
        let filter = config
            .exclude_filter(
                Some(Path::new("/project/fatou.toml")),
                Path::new("/elsewhere"),
                &[],
            )
            .unwrap()
            .with_force_exclude(true);
        assert!(filter.force_excludes(Path::new("/project/vendor/a.jl")));
        // Outside the config root, patterns cannot apply.
        assert!(!filter.force_excludes(Path::new("/elsewhere/vendor/a.jl")));
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
