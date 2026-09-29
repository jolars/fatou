//! Filesystem adapter for script analysis. The project model and its salsa
//! queries consume the resulting sources without performing disk I/O.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::incremental::normalize_path;
use crate::project::include_edges;
use crate::project::scripts::{ScriptAnalysis, ScriptSources};

/// Load an explicit program set, rejecting unreadable entry files.
pub fn analyze(entries: &[PathBuf]) -> Result<ScriptAnalysis, String> {
    let sources = load_sources(entries, &BTreeMap::new());
    for entry in entries {
        if let Some(Err(error)) = sources.get(&normalize_path(entry)) {
            return Err(format!(
                "cannot read script entry point {}: {error}",
                entry.display()
            ));
        }
    }
    Ok(ScriptAnalysis::from_sources(entries, &sources))
}

/// Load only the include closure. Open buffers override disk, including files
/// that have not been saved yet. Errors remain explicit graph inputs.
pub fn load_sources(entries: &[PathBuf], buffers: &BTreeMap<PathBuf, Arc<str>>) -> ScriptSources {
    let mut sources = BTreeMap::new();
    let mut pending = entries.to_vec();
    while let Some(path) = pending.pop() {
        let path = normalize_path(&path);
        if sources.contains_key(&path) {
            continue;
        }
        let text = buffers.get(&path).cloned().map(Ok).unwrap_or_else(|| {
            std::fs::read_to_string(&path)
                .map(Arc::from)
                .map_err(|error| error.to_string())
        });
        if let Ok(text) = &text {
            let parsed = crate::parser::parse(text);
            pending.extend(
                include_edges(&parsed.cst, path.parent())
                    .into_iter()
                    .filter_map(|edge| edge.target),
            );
        }
        sources.insert(path, text);
    }
    sources
}
