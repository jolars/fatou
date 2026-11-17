//! Filesystem adapter for script analysis. The project model and its salsa
//! queries consume the resulting sources without performing disk I/O.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::incremental::normalize_path;
use crate::project::scripts::{ScriptAnalysis, ScriptSources};

/// Load an explicit program set, rejecting unreadable entry files.
pub fn analyze(entries: &[PathBuf]) -> Result<ScriptAnalysis, String> {
    analyze_with_edition(entries, None)
}

pub fn analyze_with_edition(
    entries: &[PathBuf],
    edition: Option<crate::parser::SyntaxEdition>,
) -> Result<ScriptAnalysis, String> {
    let catalog = load_contexts(entries, &BTreeMap::new(), edition);
    let sources = &catalog.sources;
    for entry in entries {
        if let Some(Err(error)) = sources.get(&normalize_path(entry)) {
            return Err(format!(
                "cannot read script entry point {}: {error}",
                entry.display()
            ));
        }
    }
    Ok(ScriptAnalysis::from_catalog(entries, &catalog))
}

/// Load only the include closure. Open buffers override disk, including files
/// that have not been saved yet. Errors remain explicit graph inputs.
pub fn load_sources(entries: &[PathBuf], buffers: &BTreeMap<PathBuf, Arc<str>>) -> ScriptSources {
    load_contexts(entries, buffers, None).sources
}

/// Load a program set under each entry's project-selected edition. The catalog
/// retains both include membership and shared text for the analysis adapter.
pub fn load_contexts(
    entries: &[PathBuf],
    buffers: &BTreeMap<PathBuf, Arc<str>>,
    override_edition: Option<crate::parser::SyntaxEdition>,
) -> crate::parse_context::ContextCatalog {
    crate::parse_context::ContextCatalog::discover(&[], entries, buffers, override_edition)
}
