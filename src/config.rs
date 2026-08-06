//! `fatou.toml` configuration: schema, defaults, and discovery.
//!
//! Defaults follow common Julia conventions (line width 92, 4-space indent).
//! Discovery walks up from an anchor directory looking for a `fatou.toml`,
//! then falls back to [`$FATOU_CONFIG`](CONFIG_ENV_VAR) and the
//! [global user config](global_config_path).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::file_discovery::{ExcludeError, ExcludeFilter};
use crate::formatter::LineEnding;
use crate::julia_version::{VersionRange, parse_compat};
use crate::linter::Severity;

pub const CONFIG_FILE_NAME: &str = "fatou.toml";

/// Environment variable naming a config file to use when the ancestor walk
/// finds no project `fatou.toml`. Checked before the
/// [global user config](global_config_path) in [`Config::resolve`].
pub const CONFIG_ENV_VAR: &str = "FATOU_CONFIG";

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
    /// relative to [`ConfigSource::exclude_root`].
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
    /// Resolve configuration for the CLI and the language server. Precedence:
    /// an explicit `--config` path, then a project `fatou.toml` discovered by
    /// walking up from `anchor`, then a file named by
    /// [`$FATOU_CONFIG`](CONFIG_ENV_VAR), then the
    /// [global user config](global_config_path), then built-in defaults.
    /// Whole-file fallback, never a merge. `no_config` skips every file
    /// (project, env, and global).
    ///
    /// Along with the config and [where it came from](ConfigSource), returns any
    /// deprecation warnings raised while parsing (e.g. snake_case `[format]`
    /// keys).
    pub fn resolve(
        explicit: Option<&Path>,
        no_config: bool,
        anchor: &Path,
    ) -> Result<(Self, ConfigSource, Vec<String>), ConfigError> {
        Self::resolve_with_fallbacks(
            explicit,
            no_config,
            anchor,
            env_config_path().as_deref(),
            global_config_path().as_deref(),
        )
    }

    /// [`resolve`](Self::resolve) with the env and global fallback paths
    /// injected, so tests can exercise them without touching the real
    /// environment or home directory.
    fn resolve_with_fallbacks(
        explicit: Option<&Path>,
        no_config: bool,
        anchor: &Path,
        env: Option<&Path>,
        global: Option<&Path>,
    ) -> Result<(Self, ConfigSource, Vec<String>), ConfigError> {
        if no_config {
            return Ok((Self::default(), ConfigSource::None, Vec::new()));
        }
        if let Some(path) = explicit {
            let (config, warnings) = Self::load(path)?;
            return Ok((config, ConfigSource::Explicit(path.to_path_buf()), warnings));
        }
        if let Some(path) = discover(anchor) {
            let (config, warnings) = Self::load(&path)?;
            return Ok((config, ConfigSource::Discovered(path), warnings));
        }
        // A set `$FATOU_CONFIG` shadows the global config entirely, and a
        // missing or broken file is a hard error rather than a fall-through:
        // it is the config that would apply, and silently ignoring it would
        // hide a typo'd path indefinitely.
        if let Some(path) = env {
            let (config, warnings) = Self::load(path)?;
            return Ok((config, ConfigSource::Env(path.to_path_buf()), warnings));
        }
        // Same rationale: a broken global config is a hard error, not a silent
        // fall-through to built-in defaults.
        if let Some(path) = global {
            let (config, warnings) = Self::load(path)?;
            return Ok((config, ConfigSource::Global(path.to_path_buf()), warnings));
        }
        Ok((Self::default(), ConfigSource::None, Vec::new()))
    }

    /// Build the file-discovery [`ExcludeFilter`] from this config's `exclude`
    /// and `extend-exclude` plus any `extra` patterns (e.g. CLI `--exclude`).
    /// Patterns are rooted at [`ConfigSource::exclude_root`]: the directory
    /// containing a project-local config file, or `anchor` for the env and
    /// global configs and the no-config case.
    pub fn exclude_filter(
        &self,
        source: &ConfigSource,
        anchor: &Path,
        extra: &[String],
    ) -> Result<ExcludeFilter, ExcludeError> {
        let root = source.exclude_root(anchor);
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

/// Walk up from `anchor` looking for a `fatou.toml`. The env and global
/// configs are *not* consulted here; those fallbacks live in
/// [`Config::resolve`].
fn discover(anchor: &Path) -> Option<PathBuf> {
    for dir in anchor.ancestors() {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Which configuration source [`Config::resolve`] loaded, carrying its path.
///
/// The distinction matters for relative exclude patterns: a project-local file
/// anchors them at its own directory, while the env and global configs have no
/// project location and anchor at the caller's directory instead (see
/// [`exclude_root`](Self::exclude_root)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Loaded from an explicit `--config <path>`.
    Explicit(PathBuf),
    /// Discovered by the ancestor walk from the anchor directory.
    Discovered(PathBuf),
    /// Named by the [`$FATOU_CONFIG`](CONFIG_ENV_VAR) environment variable,
    /// used when no project config is discovered.
    Env(PathBuf),
    /// The global user config (e.g. `~/.config/fatou/fatou.toml`), used when no
    /// project config is discovered and `$FATOU_CONFIG` is unset.
    Global(PathBuf),
    /// No config file found; built-in defaults are in use.
    None,
}

impl ConfigSource {
    /// Path of the resolved config file, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Explicit(p) | Self::Discovered(p) | Self::Env(p) | Self::Global(p) => Some(p),
            Self::None => None,
        }
    }

    /// The directory relative exclude patterns resolve against: the config
    /// file's own directory for a project-local file, or `anchor` (the CLI
    /// working directory, or the document's directory in the LSP) for the env
    /// and global configs and the no-config case, which have no project
    /// location.
    pub fn exclude_root<'a>(&'a self, anchor: &'a Path) -> &'a Path {
        match self {
            Self::Explicit(p) | Self::Discovered(p) => p.parent().unwrap_or(anchor),
            Self::Env(_) | Self::Global(_) | Self::None => anchor,
        }
    }
}

/// Path named by the [`$FATOU_CONFIG`](CONFIG_ENV_VAR) environment variable, or
/// `None` when unset or empty (an empty value counts as unset, the usual shell
/// convention).
fn env_config_path() -> Option<PathBuf> {
    let value = std::env::var_os(CONFIG_ENV_VAR)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Path to the global user config, the fallback when no project `fatou.toml` is
/// discovered and `$FATOU_CONFIG` is unset: the first existing
/// `<dir>/fatou/fatou.toml` among the candidate directories below.
///
/// `$XDG_CONFIG_HOME` comes first everywhere — setting it is an explicit
/// opt-in. After it, `~/.config` is checked on every platform so the
/// CLI-dotfile convention works on macOS and Windows too, but it is checked
/// *after* the platform config dir on Windows: there a `~/.config` tree is
/// usually incidental (synced dotfiles, WSL interop) while `%APPDATA%` is where
/// a Windows user deliberately puts a config, so the incidental location must
/// not shadow the deliberate one. On macOS the order is reversed for the same
/// reason read the other way: `~/Library/Application Support` is a GUI-app
/// convention, and a CLI user who writes `~/.config/fatou/fatou.toml` expects it
/// to win. On Linux the two coincide (`dirs::config_dir()` is `~/.config` when
/// `$XDG_CONFIG_HOME` is unset), so the order is moot.
fn global_config_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        candidates.push(PathBuf::from(xdg));
    }
    // Windows `%APPDATA%`, macOS `~/Library/Application Support`, Linux
    // `~/.config`.
    let native = dirs::config_dir();
    let dotfile = dirs::home_dir().map(|home| home.join(".config"));
    if cfg!(windows) {
        candidates.extend(native);
        candidates.extend(dotfile);
    } else {
        candidates.extend(dotfile);
        candidates.extend(native);
    }
    candidates
        .into_iter()
        .map(|dir| dir.join("fatou").join(CONFIG_FILE_NAME))
        .find(|path| path.is_file())
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
            .exclude_filter(
                &ConfigSource::None,
                Path::new("/tmp"),
                &["cli/".to_string()],
            )
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
                &ConfigSource::Discovered(PathBuf::from("/project/fatou.toml")),
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
    fn exclude_filter_roots_global_config_at_anchor() {
        let config = Config {
            exclude: vec!["vendor/".to_string()],
            ..Config::default()
        };
        // The global config has no project location, so its patterns apply
        // relative to the caller's directory, not `~/.config/fatou`.
        let filter = config
            .exclude_filter(
                &ConfigSource::Global(PathBuf::from("/home/u/.config/fatou/fatou.toml")),
                Path::new("/project"),
                &[],
            )
            .unwrap()
            .with_force_exclude(true);
        assert!(filter.force_excludes(Path::new("/project/vendor/a.jl")));
    }

    #[test]
    fn no_config_returns_defaults() {
        let (config, source, warnings) =
            Config::resolve(None, true, Path::new("/nonexistent")).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(source, ConfigSource::None);
        assert!(warnings.is_empty());
    }

    /// `--no-config` must skip the env and global fallbacks too, not just
    /// project discovery.
    #[test]
    fn no_config_skips_env_and_global() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("env.toml");
        let global = dir.path().join("global.toml");
        std::fs::write(&env, "[format]\nline-width = 50\n").unwrap();
        std::fs::write(&global, "[format]\nline-width = 60\n").unwrap();
        let (config, source, _) =
            Config::resolve_with_fallbacks(None, true, dir.path(), Some(&env), Some(&global))
                .unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(source, ConfigSource::None);
    }

    #[test]
    fn discovered_config_wins_over_env_and_global() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join(CONFIG_FILE_NAME);
        let global = dir.path().join("global.toml");
        std::fs::write(&project, "[format]\nline-width = 70\n").unwrap();
        std::fs::write(&global, "[format]\nline-width = 60\n").unwrap();
        let (config, source, _) =
            Config::resolve_with_fallbacks(None, false, dir.path(), None, Some(&global)).unwrap();
        assert_eq!(config.format.line_width, 70);
        assert_eq!(source, ConfigSource::Discovered(project));
    }

    #[test]
    fn env_config_wins_over_global() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("env.toml");
        let global = dir.path().join("global.toml");
        std::fs::write(&env, "[format]\nline-width = 50\n").unwrap();
        std::fs::write(&global, "[format]\nline-width = 60\n").unwrap();
        let (config, source, _) =
            Config::resolve_with_fallbacks(None, false, dir.path(), Some(&env), Some(&global))
                .unwrap();
        assert_eq!(config.format.line_width, 50);
        assert_eq!(source, ConfigSource::Env(env));
    }

    #[test]
    fn global_config_applies_without_a_project_config() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        std::fs::write(&global, "[format]\nline-width = 60\n").unwrap();
        let (config, source, _) =
            Config::resolve_with_fallbacks(None, false, dir.path(), None, Some(&global)).unwrap();
        assert_eq!(config.format.line_width, 60);
        assert_eq!(source, ConfigSource::Global(global));
    }

    /// A set but dangling `$FATOU_CONFIG` (or an unparsable one) is a hard
    /// error, not a silent fall-through that would hide a typo indefinitely.
    #[test]
    fn broken_env_config_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        Config::resolve_with_fallbacks(None, false, dir.path(), Some(&missing), None)
            .expect_err("a dangling $FATOU_CONFIG must not be silently ignored");

        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "[format\n").unwrap();
        Config::resolve_with_fallbacks(None, false, dir.path(), Some(&broken), None)
            .expect_err("an unparsable $FATOU_CONFIG must not be silently ignored");
    }

    #[test]
    fn broken_global_config_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("global.toml");
        std::fs::write(&broken, "line-width = 60\n").unwrap();
        Config::resolve_with_fallbacks(None, false, dir.path(), None, Some(&broken))
            .expect_err("an invalid global config must not be silently ignored");
    }

    #[test]
    fn empty_env_var_counts_as_unset() {
        // SAFETY: single-threaded test process section; no other thread reads
        // the environment concurrently here.
        unsafe {
            std::env::set_var(CONFIG_ENV_VAR, "");
        }
        let path = env_config_path();
        unsafe {
            std::env::remove_var(CONFIG_ENV_VAR);
        }
        assert!(path.is_none());
    }
}
