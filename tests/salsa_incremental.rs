//! The salsa layer parses on demand and reparses after a text edit.

use fatou::incremental::{
    IncrementalDatabase, IncrementalDb, PrevParse, SourceFile, control_flow, file_exports,
    file_free_reads, file_qualified_reads, host_module_of, include_edges, parse_diagnostics,
    parsed_tree_root, project_declared_deps, project_graph, semantic_model,
};
use fatou::parser::{Edit, apply_edits, parse};

fn edit(range: std::ops::Range<usize>, insert: &str) -> Edit {
    Edit {
        range,
        insert: insert.to_string(),
    }
}

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
    let a = db.upsert_file(Path::new("/work/a.jl"), "f(x)\n");
    let b = db.upsert_file(Path::new("/work/./a.jl"), "g(x)\n");
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

#[test]
fn control_flow_is_reused_when_input_is_unchanged() {
    let db = IncrementalDatabase::new();
    let file = db.add_file("function f()\n    return 1\nend\n");
    let first = control_flow(&db, file);
    let second = control_flow(&db, file);
    assert!(
        std::ptr::eq(first, second),
        "same revision must return the same memoized graph"
    );
    assert_eq!(first.regions().len(), 1);
}

#[test]
fn edit_rebuilds_the_control_flow_graph() {
    let mut db = IncrementalDatabase::new();
    let file = db.add_file("function f()\n    return 1\nend\n");
    assert_eq!(control_flow(&db, file).regions().len(), 1);

    db.set_file_text(
        file,
        "function f()\n    return 1\nend\nfunction g()\n    2\nend\n",
    );
    assert_eq!(control_flow(&db, file).regions().len(), 2);
}

#[test]
fn parse_populates_and_refreshes_the_reparse_base() {
    let mut db = IncrementalDatabase::new();
    let file = db.add_file("x = 1\n");
    assert!(
        db.reparse_prev(file).is_none(),
        "no parse has run yet, so there is no reparse base"
    );

    parsed_tree_root(&db, file);
    let prev = db.reparse_prev(file).expect("first parse stores its base");
    assert_eq!(prev.text, "x = 1\n");
    assert_eq!(prev.green.to_string(), "x = 1\n");
    assert!(prev.diagnostics.is_empty());

    // An edit refreshes the base to the new parse, whichever strategy got
    // there — the base always describes the text the tree was built from.
    db.set_file_text(file, "x = 1 + 2\n");
    parsed_tree_root(&db, file);
    let prev = db.reparse_prev(file).expect("reparse base survives edits");
    assert_eq!(prev.text, "x = 1 + 2\n");
    assert_eq!(prev.green.to_string(), "x = 1 + 2\n");
}

/// Two identifier edits far apart: `diff_edit` would collapse them into one
/// span covering both statements, so this is the shape only the staged chain
/// rescues. Whichever route wins, the tree must equal a full parse.
#[test]
fn staged_edits_drive_a_scattered_reparse() {
    let mut db = IncrementalDatabase::new();
    let old = "alpha = 1\nfiller = 2\nomega = 3\n";
    let file = db.add_file(old);
    parsed_tree_root(&db, file);

    let edits = vec![edit(5..5, "X"), edit(27..27, "X")];
    let new = apply_edits(old, &edits);
    db.reparse_stage_edits(file, Some(edits));
    db.set_file_text(file, new.clone());

    assert_eq!(parsed_tree_root(&db, file).to_string(), new);
    assert_eq!(parse_diagnostics(&db, file), parse(&new).diagnostics);
    assert!(
        db.reparse_pending_edits(file).is_empty(),
        "the chain is drained once the base advances past it"
    );
}

/// The drain is unconditional, so a chain that went unused does not wedge the
/// file into rejecting forever. Without that, one stale chain would sit at the
/// head of the queue and poison every later parse.
#[test]
fn a_stale_chain_is_rejected_and_then_cleared() {
    let mut db = IncrementalDatabase::new();
    let old = "alpha = 1\nfiller = 2\nomega = 3\n";
    let file = db.add_file(old);
    parsed_tree_root(&db, file);

    // A chain that applies cleanly but does not describe how the buffer got to
    // its actual text: the reparse must not trust its offsets.
    db.reparse_stage_edits(file, Some(vec![edit(5..5, "X"), edit(27..27, "X")]));
    let actual = "alpha = 1\nfiller = 2\nomega = 3\nbeta = 4\n";
    db.set_file_text(file, actual);
    assert_eq!(parsed_tree_root(&db, file).to_string(), actual);
    assert_eq!(parse_diagnostics(&db, file), parse(actual).diagnostics);
    assert!(
        db.reparse_pending_edits(file).is_empty(),
        "a rejected chain is still drained, or it would reject every later parse"
    );

    // And the next chain, staged against the now-current base, works.
    let edits = vec![edit(5..5, "Y"), edit(28..28, "Y")];
    let new = apply_edits(actual, &edits);
    db.reparse_stage_edits(file, Some(edits));
    db.set_file_text(file, new.clone());
    assert_eq!(parsed_tree_root(&db, file).to_string(), new);
}

/// `None` means "unknown transform" (a full-buffer replacement, a disk revert):
/// the chain is dropped and the reparse falls back to diffing the two texts.
#[test]
fn staging_none_clears_the_chain() {
    let db = IncrementalDatabase::new();
    let file = db.add_file("alpha = 1\n");
    db.reparse_stage_edits(file, Some(vec![edit(5..5, "X")]));
    assert_eq!(db.reparse_pending_edits(file).len(), 1);
    db.reparse_stage_edits(file, None);
    assert!(db.reparse_pending_edits(file).is_empty());
}

/// Nothing guarantees a parse per stage — under pull diagnostics the analysis
/// thread stages on every keystroke and demands nothing. The chain must be
/// bounded where it is staged, not where it is read.
#[test]
fn an_unread_chain_stays_bounded() {
    let db = IncrementalDatabase::new();
    let file = db.add_file("alpha = 1\n");
    for i in 0..100 {
        db.reparse_stage_edits(file, Some(vec![edit(i..i, "x")]));
        assert!(
            db.reparse_pending_edits(file).len() <= 16,
            "chain grew past its budget at keystroke {i}"
        );
    }

    // A single oversized paste blows the byte budget on its own.
    db.reparse_stage_edits(file, None);
    db.reparse_stage_edits(file, Some(vec![edit(0..0, &"x".repeat(65 * 1024))]));
    assert!(db.reparse_pending_edits(file).is_empty());
}

/// The base cache is bounded. Every file the include graph reaches parses, but
/// only the few the editor is touching ever score a hit — without a bound, a
/// project-wide analysis would pin a second copy of every source file's text
/// plus its green tree for the life of the session.
#[test]
fn the_reparse_base_cache_is_bounded() {
    let mut db = IncrementalDatabase::new();
    let files: Vec<_> = (0..200)
        .map(|i| db.add_file(format!("x{i} = {i}\n")))
        .collect();
    for &file in &files {
        parsed_tree_root(&db, file);
    }

    let cached = files
        .iter()
        .filter(|&&f| db.reparse_prev(f).is_some())
        .count();
    assert!(
        cached <= 64,
        "the base cache grew to {cached} entries across 200 files"
    );
    assert!(
        db.reparse_prev(*files.last().unwrap()).is_some(),
        "the most recently parsed file must keep its base"
    );
    assert!(
        db.reparse_prev(files[0]).is_none(),
        "the least recently used base should have been evicted"
    );

    // Eviction is a performance decision only. The evicted file's parse is
    // still memoized, so reading it costs nothing; and once its text changes,
    // the query reruns, full-parses (no base to splice against), and
    // repopulates a base for next time.
    assert_eq!(parsed_tree_root(&db, files[0]).to_string(), "x0 = 0\n");
    db.set_file_text(files[0], "x0 = 1\n");
    assert_eq!(parsed_tree_root(&db, files[0]).to_string(), "x0 = 1\n");
    assert_eq!(
        parse_diagnostics(&db, files[0]),
        parse("x0 = 1\n").diagnostics
    );
    assert!(
        db.reparse_prev(files[0]).is_some(),
        "a reparsed file repopulates its base"
    );
}

/// A `project_graph` or `workspace_reference_index` sweep parses far more files
/// than the cache holds, and none of them will ever score a hit. Under a plain
/// LRU it would evict every open buffer's base in one pass, and the next
/// keystroke would full-parse.
#[test]
fn a_sweep_does_not_evict_an_edited_buffer() {
    let mut db = IncrementalDatabase::new();
    let old = "alpha = 1\n";
    let buffer = db.add_file(old);

    // Typing into the buffer is what marks it hot.
    let edits = vec![edit(5..5, "X")];
    let new = apply_edits(old, &edits);
    db.reparse_stage_edits(buffer, Some(edits));
    db.set_file_text(buffer, new);
    parsed_tree_root(&db, buffer);
    assert!(db.reparse_prev(buffer).is_some(), "the buffer has a base");

    // Now a project-wide sweep, well past the budget.
    let swept: Vec<_> = (0..200)
        .map(|i| db.add_file(format!("x{i} = {i}\n")))
        .collect();
    for &file in &swept {
        parsed_tree_root(&db, file);
    }

    assert!(
        db.reparse_prev(buffer).is_some(),
        "a sweep of files that can never hit must not cost the buffer its base"
    );
}

/// Preferring cold entries is a tie-break, not an exemption: past
/// `MAX_REPARSE_BASES` *hot* files the budget still binds, and the oldest of
/// them goes. No editor holds that many buffers, but the bound is what keeps
/// the cache bounded at all.
#[test]
fn the_bound_still_holds_when_every_file_is_hot() {
    let db = IncrementalDatabase::new();
    let files: Vec<_> = (0..200)
        .map(|i| db.add_file(format!("x{i} = {i}\n")))
        .collect();
    // Staging is enough to make a file hot; no parse is needed to fill the
    // cache, and the surviving chain is what marks an entry as still resident.
    for &file in &files {
        db.reparse_stage_edits(file, Some(vec![edit(0..0, " ")]));
    }

    let resident = files
        .iter()
        .filter(|&&f| !db.reparse_pending_edits(f).is_empty())
        .count();
    assert!(
        resident <= 64,
        "the cache grew to {resident} entries across 200 hot files"
    );
    assert!(
        !db.reparse_pending_edits(*files.last().unwrap()).is_empty(),
        "the most recently staged file must survive"
    );
    assert!(
        db.reparse_pending_edits(files[0]).is_empty(),
        "the least recently used hot entry should have been evicted"
    );
}

/// A stage that lands between a parse's peek and its store must survive: the
/// store drains only the prefix that parse actually saw.
#[test]
fn a_stage_after_a_peek_survives_the_drain() {
    let db = IncrementalDatabase::new();
    let file = db.add_file("alpha = 1\n");
    db.reparse_stage_edits(file, Some(vec![edit(5..5, "X")]));

    let peeked = db.reparse_pending_edits(file);
    assert_eq!(peeked.len(), 1);
    // ... the analysis thread stages another edit here, concurrently ...
    db.reparse_stage_edits(file, Some(vec![edit(6..6, "Y")]));

    db.reparse_store(
        file,
        PrevParse {
            text: "alphaX = 1\n".to_string(),
            green: parse("alphaX = 1\n").cst.green().to_owned(),
            diagnostics: Vec::new(),
        },
        None,
        peeked.len(),
    );
    assert_eq!(
        db.reparse_pending_edits(file),
        vec![edit(6..6, "Y")],
        "only the peeked prefix is drained"
    );
}

/// A downstream query that counts its own executions, to observe backdating.
#[salsa::tracked(returns(copy))]
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
#[salsa::tracked(returns(copy))]
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
        #[salsa::tracked(returns(copy))]
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

/// The probes for the project-file firewall: a `Project.toml` edit must reach
/// the declared dependencies and nothing else. `parsed_document` is `no_eq`, so
/// a parse that re-executed would force `probe_parse` to re-run — a counter
/// resting at 1 is a sound witness that it did not.
#[salsa::tracked(returns(copy))]
fn probe_parse(db: &dyn IncrementalDb, file: SourceFile) -> usize {
    use std::sync::atomic::Ordering;
    PARSE_PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
    parse_diagnostics(db, file).len()
}

static PARSE_PROBE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[salsa::tracked(returns(copy))]
fn probe_graph(db: &dyn IncrementalDb) -> usize {
    use std::sync::atomic::Ordering;
    GRAPH_PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
    project_graph(db).nodes.len()
}

static GRAPH_PROBE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A one-member workspace package with its project file tracked as an input,
/// returned as `(db, entry, project)`.
fn project_db(project_text: &str) -> (IncrementalDatabase, SourceFile, SourceFile) {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use fatou::index::model::{DefLocation, Span};
    use fatou::index::{ModuleIndex, PackageIndex};

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
            usings: Vec::new(),
            imported_names: Vec::new(),
        },
        members: Vec::new(),
        member_modules: Default::default(),
        diagnostics: Vec::new(),
    };

    let mut db = IncrementalDatabase::new();
    let entry = db.upsert_file(
        Path::new("/work/MyPkg/src/MyPkg.jl"),
        "module MyPkg\nk() = 1\nend\n".to_string(),
    );
    // Tracked before `set_project_files`, which is create-or-return: the input
    // stands in for an open editor buffer that never touched disk.
    let project = db.upsert_file(
        Path::new("/work/MyPkg/Project.toml"),
        project_text.to_string(),
    );

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(pkg));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, vec!["MyPkg".to_string()]);
    db.set_workspace_files(vec![entry]);
    db.set_project_files(BTreeMap::from([(
        "MyPkg".to_string(),
        PathBuf::from("/work/MyPkg/Project.toml"),
    )]));
    (db, entry, project)
}

fn declared(db: &IncrementalDatabase) -> Vec<String> {
    db.declared_deps("MyPkg")
        .map(|deps| deps.iter().cloned().collect())
        .unwrap_or_default()
}

/// The mirror of the body-edit firewall, from the other side: a `Project.toml`
/// edit must reach the resolution layer and leave the parses (and the include
/// graph they feed) alone. Without this, making the project file a live input
/// trades a body-edit firewall for a project-edit stampede.
#[test]
fn a_project_file_edit_invalidates_deps_but_not_parses() {
    use std::sync::atomic::Ordering;

    let (mut db, entry, project) =
        project_db("[deps]\nFoo = \"11111111-2222-3333-4444-555555555555\"\n");

    assert_eq!(probe_parse(&db, entry), 0);
    let graph_before = probe_graph(&db);
    assert_eq!(PARSE_PROBE_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(GRAPH_PROBE_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(declared(&db), ["Foo"]);

    db.set_file_text(
        project,
        "[deps]\nFoo = \"11111111-2222-3333-4444-555555555555\"\nBar = \"66666666-7777-8888-9999-aaaaaaaaaaaa\"\n",
    );

    // Witness: the edit really landed on the resolution layer.
    assert_eq!(declared(&db), ["Bar", "Foo"]);
    assert_eq!(probe_parse(&db, entry), 0);
    assert_eq!(probe_graph(&db), graph_before);
    assert_eq!(
        PARSE_PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "a project-file edit must not re-parse a single Julia file"
    );
    assert_eq!(
        GRAPH_PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "a project-file edit must not re-derive the include graph"
    );
}

/// A probe over the project file's own firewall, to observe backdating.
#[salsa::tracked(returns(copy))]
fn probe_deps(db: &dyn IncrementalDb, file: SourceFile) -> usize {
    use std::sync::atomic::Ordering;
    DEPS_PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
    project_declared_deps(db, file).map_or(0, |deps| deps.len())
}

static DEPS_PROBE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The other half of the firewall: an edit *within* the project file that
/// leaves `[deps]` alone backdates, so a consumer of the declared set rests.
#[test]
fn a_compat_only_project_edit_backdates_declared_deps() {
    use std::sync::atomic::Ordering;

    let (mut db, _, project) =
        project_db("[deps]\nFoo = \"11111111-2222-3333-4444-555555555555\"\n");

    assert_eq!(probe_deps(&db, project), 1);
    assert_eq!(DEPS_PROBE_RUNS.load(Ordering::SeqCst), 1);

    db.set_file_text(
        project,
        "[deps]\nFoo = \"11111111-2222-3333-4444-555555555555\"\n\n[compat]\njulia = \"1.10\"\n",
    );

    // Witness: the file's text really changed, so the query re-ran.
    assert!(db.file_text(project).text().contains("[compat]"));
    assert_eq!(probe_deps(&db, project), 1);
    assert_eq!(
        DEPS_PROBE_RUNS.load(Ordering::SeqCst),
        1,
        "an edit that leaves `[deps]` alone must backdate rather than re-derive"
    );
}

/// A project file that stops parsing takes the declared set with it, rather
/// than reporting a half-typed `[deps]` table as the truth: `unresolved-import`
/// goes quiet for a keystroke instead of flashing a false finding.
#[test]
fn a_broken_project_file_declares_nothing() {
    let (db, _, _) = project_db("[deps]\nFoo = \n");
    assert_eq!(db.declared_deps("MyPkg"), None);
}

/// A probe over the per-file host-module firewall, to observe backdating.
#[salsa::tracked(returns(copy))]
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
            usings: Vec::new(),
            imported_names: Vec::new(),
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
    db.set_library(packages, roots, vec!["MyPkg".to_string()]);
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
