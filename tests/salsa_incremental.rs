//! The salsa layer parses on demand and reparses after a text edit.

use fatou::incremental::{
    IncrementalDatabase, IncrementalDb, SourceFile, file_exports, file_free_reads,
    file_qualified_reads, include_edges, parse_diagnostics, parsed_tree_root, semantic_model,
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

// --- firewall queries: backdating across a *position-shifting* edit ----------
//
// The tests above only cover same-width edits (the whole model backdates). The
// firewall's real job is the harder case: an edit that shifts ranges — widening
// a function body — invalidates the range-carrying `semantic_model`, yet the
// range-free projection is unchanged, so it backdates and dependents are spared.

/// The file-scope range spans the whole file, so its length tracks the text
/// length: a witness that a length-changing edit really did re-run (and *not*
/// backdate) `semantic_model`, making each firewall test non-vacuous.
fn file_span_len(db: &IncrementalDatabase, file: SourceFile) -> u32 {
    u32::from(semantic_model(db, file).scopes()[0].range.len())
}

macro_rules! firewall_backdates_test {
    ($test:ident, $probe:ident, $counter:ident, $query:ident, $seed:expr, $edited:expr) => {
        #[salsa::tracked]
        fn $probe(db: &dyn IncrementalDb, file: SourceFile) -> usize {
            use std::sync::atomic::Ordering;
            $counter.fetch_add(1, Ordering::SeqCst);
            $query(db, file).len()
        }

        static $counter: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        #[test]
        fn $test() {
            use std::sync::atomic::Ordering;

            let mut db = IncrementalDatabase::new();
            let file = db.add_file($seed);
            let before = $probe(&db, file);
            assert_eq!($counter.load(Ordering::SeqCst), 1);
            let span_before = file_span_len(&db, file);

            // Widen a function body: every range past it shifts, so the model is
            // not structurally equal and re-runs — but the firewall projection is
            // unchanged, so it backdates and the probe is not re-run.
            db.set_file_text(file, $edited);
            assert_ne!(
                file_span_len(&db, file),
                span_before,
                "the edit changed the file length, so the model re-ran"
            );
            assert_eq!($probe(&db, file), before, "the projection is unchanged");
            assert_eq!(
                $counter.load(Ordering::SeqCst),
                1,
                "the range-free projection must backdate across a position shift"
            );
        }
    };
}

firewall_backdates_test!(
    position_shift_backdates_file_exports,
    probe_exports,
    EXPORTS_PROBE_RUNS,
    file_exports,
    "f() = 1\nz = 2\n",
    "f() = 111\nz = 2\n"
);

firewall_backdates_test!(
    position_shift_backdates_file_free_reads,
    probe_free_reads,
    FREE_READS_PROBE_RUNS,
    file_free_reads,
    "g() = 1\ny = sin(x)\n",
    "g() = 111\ny = sin(x)\n"
);

firewall_backdates_test!(
    position_shift_backdates_file_qualified_reads,
    probe_qualified_reads,
    QUALIFIED_READS_PROBE_RUNS,
    file_qualified_reads,
    "h() = 1\nz = A.b.c\n",
    "h() = 111\nz = A.b.c\n"
);

firewall_backdates_test!(
    position_shift_backdates_include_edges,
    probe_include_edges,
    INCLUDE_EDGES_PROBE_RUNS,
    include_edges,
    "k() = 1\ninclude(\"a.jl\")\n",
    "k() = 111\ninclude(\"a.jl\")\n"
);
