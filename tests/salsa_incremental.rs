//! The salsa layer parses on demand and reparses after a text edit.

use fatou::incremental::{
    IncrementalDatabase, IncrementalDb, SourceFile, file_exports, file_free_reads,
    file_qualified_reads, host_module_of, include_edges, parse_diagnostics, parsed_tree_root,
    project_graph, semantic_model,
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

/// A probe over the per-file host-module firewall, to observe backdating.
#[salsa::tracked]
fn probe_host_module(db: &dyn IncrementalDb, file: SourceFile) -> usize {
    use std::sync::atomic::Ordering;
    HOST_PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
    host_module_of(db, file).len()
}

static HOST_PROBE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn host_module_of_backdates_across_graph_rederivations() {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use fatou::index::model::{DefLocation, Span};
    use fatou::index::{ModuleIndex, PackageIndex};

    // A minimal workspace package: the graph derives hosts from include edges,
    // so the index itself can stay empty.
    let pkg = PackageIndex {
        name: "MyPkg".to_string(),
        root: ModuleIndex {
            name: "MyPkg".to_string(),
            bare: false,
            loc: DefLocation {
                file: "src/MyPkg.jl".into(),
                range: Span { start: 0, end: 0 },
            },
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        },
        members: Vec::new(),
        member_modules: Default::default(),
        diagnostics: Vec::new(),
    };

    let mut db = IncrementalDatabase::new();
    let entry = db.upsert_file(
        Path::new("/work/MyPkg/src/MyPkg.jl"),
        "module MyPkg\nk() = 1\nmodule Sub\ninclude(\"b.jl\")\nend\nend\n".to_string(),
    );
    let b = db.upsert_file(Path::new("/work/MyPkg/src/b.jl"), "x = 1\n".to_string());

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(pkg));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));
    db.set_workspace_files(vec![entry, b]);

    assert_eq!(probe_host_module(&db, b), 1, "b.jl hosts inside Sub");
    assert_eq!(HOST_PROBE_RUNS.load(Ordering::SeqCst), 1);

    // A body edit in the entry that shifts positions but keeps the include
    // structure: `include_edges` backdates, so the graph is not even re-derived
    // and the probe rests.
    db.set_file_text(
        entry,
        "module MyPkg\nk() = 111\nmodule Sub\ninclude(\"b.jl\")\nend\nend\n".to_string(),
    );
    assert_eq!(probe_host_module(&db, b), 1);
    assert_eq!(
        HOST_PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "a position shift must not reach past the include-edge firewall"
    );

    // An include-structure edit: the graph re-derives (c.jl joins the closure),
    // `host_module_of(b)` re-runs but returns an equal host — it backdates and
    // the probe is still not re-run.
    let c = db.upsert_file(Path::new("/work/MyPkg/src/c.jl"), "y = 2\n".to_string());
    db.set_file_text(
        entry,
        "module MyPkg\nk() = 111\ninclude(\"c.jl\")\nmodule Sub\ninclude(\"b.jl\")\nend\nend\n"
            .to_string(),
    );
    db.set_workspace_files(vec![entry, b, c]);
    assert!(
        project_graph(&db).nodes.iter().any(|p| p.ends_with("c.jl")),
        "witness: the graph really re-derived with c.jl in the closure"
    );
    assert_eq!(probe_host_module(&db, b), 1);
    assert_eq!(
        HOST_PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "an unchanged per-file host must backdate across a graph re-derivation"
    );
}
