//! Content-derived `resultId`s for pull responses.
//!
//! Both pull diagnostics and semantic tokens carry a `resultId` the client
//! echoes back on its next request for the same document. When the freshly
//! computed findings hash to the id the client already holds, the findings are
//! unchanged since its last pull, so the server answers `Unchanged`
//! (diagnostics) or an empty delta (semantic tokens) instead of resending the
//! payload — a bandwidth win for the common re-pull-without-an-edit case.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::Serialize;

/// A stable, content-derived id for `value`: the hex SipHash of its JSON
/// serialization. `DefaultHasher` is seeded with fixed keys, so the id is
/// deterministic across pulls (and process restarts) for identical findings —
/// exactly the property the `Unchanged`/empty-delta shortcut relies on.
pub(crate) fn content_hash<T: Serialize>(value: &T) -> String {
    let mut hasher = DefaultHasher::new();
    // Findings aren't all `Hash` (a `Diagnostic` carries free-form `data`
    // JSON), so hash a canonical serialization rather than the value itself.
    match serde_json::to_vec(value) {
        Ok(bytes) => bytes.hash(&mut hasher),
        // Serialization can't realistically fail for these types; a distinct
        // sentinel keeps a failure from colliding with the empty-findings hash.
        Err(_) => u64::MAX.hash(&mut hasher),
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_values_hash_equal_and_distinct_values_differ() {
        let a = vec![1u32, 2, 3];
        let b = vec![1u32, 2, 3];
        let c = vec![1u32, 2, 4];
        assert_eq!(content_hash(&a), content_hash(&b));
        assert_ne!(content_hash(&a), content_hash(&c));
    }

    /// The empty findings case still yields a stable id (not the error
    /// sentinel), so an unchanged empty file re-pulls as `Unchanged`.
    #[test]
    fn empty_findings_hash_is_stable() {
        let empty: Vec<u32> = Vec::new();
        assert_eq!(content_hash(&empty), content_hash(&empty));
    }
}
