//! Per-document configuration resolution: `fatou.toml` discovery shadowing
//! editor-pushed settings.
//!
//! The main loop owns a [`ConfigStore`] and resolves configuration at each
//! dispatch site (formatting, linting, code actions), so the resolved style
//! and rules travel with the request and no other thread shares config state.
//! Precedence is full shadowing, the rule ruff and air follow: a discovered
//! `fatou.toml` (merged over built-in defaults) is the document's config and
//! editor-pushed settings are ignored entirely; without one, the client
//! settings (merged over defaults) apply; without those, the defaults.
//!
//! Client settings arrive as JSON from `initializationOptions` and
//! `workspace/didChangeConfiguration`, parsed through the same [`RawConfig`]
//! schema as the TOML file (bare, or wrapped under a `"fatou"` key the way VS
//! Code namespaces settings). Discovery results are cached per parent
//! directory and invalidated wholesale when any `fatou.toml` changes or the
//! client pushes new settings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lsp_types::Uri;

use crate::config::{Config, RawConfig};
use crate::formatter::FormatStyle;

use super::lint::ServerRules;
use super::uri;

/// One resolved configuration: everything a dispatch site needs, derived once
/// per load (the lint dispatch table included) rather than per request.
pub(crate) struct ResolvedConfig {
    pub(crate) style: FormatStyle,
    pub(crate) rules: Arc<ServerRules>,
}

impl ResolvedConfig {
    fn resolve(config: &Config, warnings: &mut Vec<String>) -> Arc<Self> {
        let (rules, unknown) = ServerRules::from_config(&config.lint);
        warnings.extend(
            unknown
                .into_iter()
                .map(|id| format!("unknown rule `{id}` in select/ignore/severity")),
        );
        Arc::new(Self {
            style: FormatStyle::from(&config.format),
            rules: Arc::new(rules),
        })
    }
}

/// The main loop's configuration state: the client-settings layer plus a cache
/// of per-directory discovery results.
pub(crate) struct ConfigStore {
    /// Client settings merged over defaults; the fallback when discovery finds
    /// no `fatou.toml`.
    client: Arc<ResolvedConfig>,
    /// Discovery results keyed by the document's parent directory. A negative
    /// result (no `fatou.toml` above the directory) caches the client config,
    /// so the ancestor walk and file reads run once per directory, not per
    /// keystroke. Cleared wholesale on any invalidation.
    by_dir: HashMap<PathBuf, Arc<ResolvedConfig>>,
}

impl ConfigStore {
    /// Build the store from the client's `initializationOptions`, returning
    /// warnings to log (unparsable settings, deprecated keys, unknown rules).
    /// Unparsable or absent options leave the defaults in place.
    pub(crate) fn new(initialization_options: Option<serde_json::Value>) -> (Self, Vec<String>) {
        let mut store = Self {
            client: ResolvedConfig::resolve(&Config::default(), &mut Vec::new()),
            by_dir: HashMap::new(),
        };
        let warnings = match initialization_options {
            Some(options) => store.set_client_settings(options),
            None => Vec::new(),
        };
        (store, warnings)
    }

    /// Replace the client-settings layer from a `didChangeConfiguration`
    /// payload (or `initializationOptions`), returning warnings to log. A
    /// `null` payload keeps the current settings — some clients send it as a
    /// bare "configuration changed" ping — but still drops the discovery
    /// cache, so changed `fatou.toml` contents are re-read. Unparsable
    /// settings keep the previous ones (with a warning) rather than silently
    /// reverting to defaults.
    pub(crate) fn set_client_settings(&mut self, settings: serde_json::Value) -> Vec<String> {
        self.by_dir.clear();
        if settings.is_null() {
            return Vec::new();
        }
        // VS Code namespaces settings under the section name; accept both the
        // wrapped and the bare shape.
        let payload = match settings.get("fatou") {
            Some(section) => section.clone(),
            None => settings,
        };
        match serde_json::from_value::<RawConfig>(payload) {
            Ok(raw) => {
                let (config, mut warnings) = raw.into_config();
                self.client = ResolvedConfig::resolve(&config, &mut warnings);
                warnings
            }
            Err(err) => vec![format!("invalid client settings (kept previous): {err}")],
        }
    }

    /// A watched `fatou.toml` changed on disk: drop the discovery cache so the
    /// next resolution re-reads it.
    pub(crate) fn invalidate_discovered(&mut self) {
        self.by_dir.clear();
    }

    /// Resolve the configuration for a document. Warnings are non-empty only
    /// when a cache miss loaded a `fatou.toml` (so each load logs once, not
    /// per request). A non-`file:` URI (an untitled buffer) has no directory
    /// to anchor discovery and gets the client config.
    pub(crate) fn for_uri(&mut self, uri: &Uri) -> (Arc<ResolvedConfig>, Vec<String>) {
        let Some(dir) = uri::to_path(uri).and_then(|path| path.parent().map(PathBuf::from)) else {
            return (Arc::clone(&self.client), Vec::new());
        };
        if let Some(cached) = self.by_dir.get(&dir) {
            return (Arc::clone(cached), Vec::new());
        }
        let mut warnings = Vec::new();
        let resolved = match Config::resolve(None, false, &dir) {
            Ok((config, Some(source), deprecations)) => {
                warnings.extend(
                    deprecations
                        .into_iter()
                        .map(|warning| format!("{}: {warning}", source.display())),
                );
                ResolvedConfig::resolve(&config, &mut warnings)
            }
            // No `fatou.toml` above the directory: the client settings apply.
            Ok((_, None, _)) => Arc::clone(&self.client),
            // Unreadable or unparsable file: warn once and keep the server
            // working on the client settings until the file is fixed (its
            // change event drops this cached fallback).
            Err(err) => {
                warnings.push(err.to_string());
                Arc::clone(&self.client)
            }
        };
        self.by_dir.insert(dir, Arc::clone(&resolved));
        (resolved, warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn uri_for(path: &std::path::Path) -> Uri {
        uri::from_path(path).unwrap()
    }

    fn store_with(settings: serde_json::Value) -> (ConfigStore, Vec<String>) {
        ConfigStore::new(Some(settings))
    }

    #[test]
    fn defaults_without_options_or_file() {
        let (mut store, warnings) = ConfigStore::new(None);
        assert_eq!(warnings, Vec::<String>::new());
        let dir = tempfile::tempdir().unwrap();
        let (config, warnings) = store.for_uri(&uri_for(&dir.path().join("a.jl")));
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(config.style, FormatStyle::default());
    }

    #[test]
    fn client_settings_apply_bare_and_wrapped() {
        let bare = serde_json::json!({"format": {"indent-width": 2}});
        let (mut store, warnings) = store_with(bare);
        assert_eq!(warnings, Vec::<String>::new());
        let dir = tempfile::tempdir().unwrap();
        let (config, _) = store.for_uri(&uri_for(&dir.path().join("a.jl")));
        assert_eq!(config.style.indent_width, 2);

        let wrapped = serde_json::json!({"fatou": {"format": {"indent-width": 3}}});
        let (mut store, warnings) = store_with(wrapped);
        assert_eq!(warnings, Vec::<String>::new());
        let (config, _) = store.for_uri(&uri_for(&dir.path().join("a.jl")));
        assert_eq!(config.style.indent_width, 3);
    }

    #[test]
    fn discovered_fatou_toml_shadows_client_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fatou.toml"),
            "[format]\nindent-width = 2\n",
        )
        .unwrap();
        let (mut store, _) = store_with(serde_json::json!({"format": {"indent-width": 8}}));
        let (config, warnings) = store.for_uri(&uri_for(&dir.path().join("a.jl")));
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(config.style.indent_width, 2);
    }

    #[test]
    fn discovery_walks_up_from_a_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fatou.toml"), "[format]\nline-width = 60\n").unwrap();
        let nested = dir.path().join("src").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        let (mut store, _) = ConfigStore::new(None);
        let (config, _) = store.for_uri(&uri_for(&nested.join("a.jl")));
        assert_eq!(config.style.line_width, 60);
    }

    #[test]
    fn invalidation_drops_the_discovery_cache() {
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("fatou.toml");
        std::fs::write(&toml, "[format]\nindent-width = 2\n").unwrap();
        let (mut store, _) = ConfigStore::new(None);
        let uri = uri_for(&dir.path().join("a.jl"));
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 2);

        // The cache serves the old config until invalidated.
        std::fs::write(&toml, "[format]\nindent-width = 7\n").unwrap();
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 2);
        store.invalidate_discovered();
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 7);
    }

    #[test]
    fn new_client_settings_replace_the_old_and_null_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let uri = uri_for(&dir.path().join("a.jl"));
        let (mut store, _) = store_with(serde_json::json!({"format": {"indent-width": 2}}));
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 2);

        let warnings =
            store.set_client_settings(serde_json::json!({"format": {"indent-width": 6}}));
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 6);

        let warnings = store.set_client_settings(serde_json::Value::Null);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 6);
    }

    #[test]
    fn invalid_client_settings_warn_and_keep_previous() {
        let dir = tempfile::tempdir().unwrap();
        let uri = uri_for(&dir.path().join("a.jl"));
        let (mut store, _) = store_with(serde_json::json!({"format": {"indent-width": 2}}));
        let warnings = store.set_client_settings(serde_json::json!({"formt": {}}));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("invalid client settings"));
        assert_eq!(store.for_uri(&uri).0.style.indent_width, 2);
    }

    #[test]
    fn broken_fatou_toml_warns_and_falls_back_to_client_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fatou.toml"), "not toml [").unwrap();
        let (mut store, _) = store_with(serde_json::json!({"format": {"indent-width": 8}}));
        let uri = uri_for(&dir.path().join("a.jl"));
        let (config, warnings) = store.for_uri(&uri);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("failed to parse"));
        assert_eq!(config.style.indent_width, 8);
        // The fallback is cached: no repeated warning per request.
        let (_, warnings) = store.for_uri(&uri);
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn unknown_rules_and_deprecated_keys_warn_once_per_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fatou.toml"),
            "[format]\nline_width = 80\n\n[lint]\nignore = [\"no-such-rule\"]\n",
        )
        .unwrap();
        let (mut store, _) = ConfigStore::new(None);
        let uri = uri_for(&dir.path().join("a.jl"));
        let (config, warnings) = store.for_uri(&uri);
        assert_eq!(config.style.line_width, 80);
        assert!(
            warnings.iter().any(|w| w.contains("line_width")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("no-such-rule")),
            "{warnings:?}"
        );
        assert_eq!(store.for_uri(&uri).1, Vec::<String>::new());
    }

    #[test]
    fn a_non_file_uri_gets_the_client_settings() {
        let (mut store, _) = store_with(serde_json::json!({"format": {"indent-width": 2}}));
        let untitled = Uri::from_str("untitled:Untitled-1").unwrap();
        let (config, warnings) = store.for_uri(&untitled);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(config.style.indent_width, 2);
    }
}
