//! The salsa layer parses on demand and reparses after a text edit.

use fatou::incremental::{
    IncrementalDatabase, IncrementalDb, SourceFile, parse_diagnostics, parsed_tree_root,
    semantic_model,
};

#[test]
fn edit_invalidates_and_reparses() {
    let mut db = IncrementalDatabase::new();
    let file = db.add_file("x = 1\n");
    assert_eq!(parsed_tree_root(&db, file).to_string(), "x = 1\n");

    db.set_file_text(file, "x = 1 + 2\n");
    assert_eq!(parsed_tree_root(&db, file).to_string(), "x = 1 + 2\n");
    assert!(parse_diagnostics(&db, file).is_empty());
}

#[test]
fn upsert_reuses_input_for_equivalent_paths() {
    use std::path::Path;

    let mut db = IncrementalDatabase::new();
    let a = db.upsert_file(Path::new("/work/a.jl"), "f(x)\n".into());
    let b = db.upsert_file(Path::new("/work/./a.jl"), "g(x)\n".into());
    assert!(a == b, "equivalent path spellings should reuse one input");
    assert_eq!(parsed_tree_root(&db, a).to_string(), "g(x)\n");
}

#[test]
fn semantic_model_is_reused_when_input_is_unchanged() {
    let db = IncrementalDatabase::new();
    let file = db.add_file("x = 1\ny = x\n");
    let first = semantic_model(&db, file);
    let second = semantic_model(&db, file);
    assert!(
        std::ptr::eq(first, second),
        "same revision must return the same memoized model"
    );
    assert_eq!(first.bindings().len(), 2);
}

#[test]
fn edit_rebuilds_the_semantic_model() {
    let mut db = IncrementalDatabase::new();
    let file = db.add_file("x = 1\n");
    assert_eq!(semantic_model(&db, file).bindings().len(), 1);

    db.set_file_text(file, "x = 1\ny = x\n");
    let model = semantic_model(&db, file);
    assert_eq!(model.bindings().len(), 2);
    let x = model.bindings().iter().position(|b| b.name == "x").unwrap();
    assert!(model.bindings()[x].read, "the edit's read is picked up");
}

/// A downstream query that counts its own executions, to observe backdating.
#[salsa::tracked]
fn probe_binding_count(db: &dyn IncrementalDb, file: SourceFile) -> usize {
    use std::sync::atomic::Ordering;
    PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
    semantic_model(db, file).bindings().len()
}

static PROBE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn same_shape_edit_backdates_the_semantic_model() {
    use std::sync::atomic::Ordering;

    let mut db = IncrementalDatabase::new();
    let file = db.add_file("x = 1\ny = x\n");
    assert_eq!(probe_binding_count(&db, file), 2);
    assert_eq!(PROBE_RUNS.load(Ordering::SeqCst), 1);

    // Only a literal changes and every range keeps its width, so the
    // rebuilt model is structurally identical: salsa backdates it and the
    // dependent query is not re-run. (Edits that shift ranges do invalidate
    // the whole model; the range-free firewall projections of TODO.md
    // Phase 2 are the barrier that survives those.)
    db.set_file_text(file, "x = 2\ny = x\n");
    assert_eq!(probe_binding_count(&db, file), 2);
    assert_eq!(
        PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "the Eq model must backdate: dependents do not re-run"
    );
}

/// A downstream query over the import model, to observe backdating.
#[salsa::tracked]
fn probe_load_count(db: &dyn IncrementalDb, file: SourceFile) -> usize {
    use std::sync::atomic::Ordering;
    LOAD_PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
    semantic_model(db, file).module_loads().len()
}

static LOAD_PROBE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn same_shape_edit_backdates_the_import_model() {
    use std::sync::atomic::Ordering;

    let mut db = IncrementalDatabase::new();
    let file = db.add_file("using A: f\nx = 1\nf(x)\n");
    assert_eq!(probe_load_count(&db, file), 1);
    assert_eq!(LOAD_PROBE_RUNS.load(Ordering::SeqCst), 1);

    // A same-width literal edit after the import leaves every range (and so
    // the whole model, loaded-modules list included) structurally equal.
    db.set_file_text(file, "using A: f\nx = 2\nf(x)\n");
    assert_eq!(probe_load_count(&db, file), 1);
    assert_eq!(
        LOAD_PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "the Eq model must backdate: dependents do not re-run"
    );
}
