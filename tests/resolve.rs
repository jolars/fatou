//! End-to-end name resolution through the read-only [`Analysis`] snapshot: the
//! shared masking order wired to the real (fallback) Base/Core system index and
//! a harvested-style package in the `LibraryIndex` salsa input.

use std::sync::Arc;

use fatou::incremental::IncrementalDatabase;
use fatou::index::build_system_index;
use fatou::index::model::{DefLocation, ExportedName, ModuleIndex, PackageIndex, Span, Visibility};
use fatou::resolve::{Namespace, Resolution};
use rowan::TextSize;

fn loc() -> DefLocation {
    DefLocation {
        file: "src/Greetings.jl".into(),
        range: Span { start: 0, end: 0 },
    }
}

/// A package whose root module exports `exports`.
fn package(name: &str, exports: &[&str]) -> Arc<PackageIndex> {
    Arc::new(PackageIndex {
        name: name.to_string(),
        root: ModuleIndex {
            name: name.to_string(),
            bare: false,
            loc: loc(),
            exports: exports
                .iter()
                .map(|n| ExportedName {
                    name: n.to_string(),
                    visibility: Visibility::Exported,
                    loc: loc(),
                })
                .collect(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        },
        diagnostics: Vec::new(),
    })
}

/// The byte offset just past the last occurrence of `needle`.
fn after(src: &str, needle: &str) -> TextSize {
    TextSize::from((src.rfind(needle).unwrap() + needle.len()) as u32)
}

#[test]
fn resolves_across_locals_using_and_base() {
    let mut db = IncrementalDatabase::new();
    // The fallback system index gives a real Base/Core export floor.
    db.set_library_packages(build_system_index(None));
    db.set_package_index("Greetings", package("Greetings", &["greet"]));

    let src = "using Greetings\nfunction f(x)\n    greet(x)\n    println(x)\nend";
    let file = db.add_file(src);
    let analysis = db.snapshot();

    // A parameter (tier 1).
    let x = after(src, "greet(x");
    assert!(matches!(
        analysis.resolve_name(file, "x", x, Namespace::Value),
        Resolution::Binding(_)
    ));

    // A `using`'d export (tier 3).
    assert_eq!(
        analysis.resolve_name(file, "greet", after(src, "greet"), Namespace::Value),
        Resolution::Using {
            module: "Greetings".into(),
            name: "greet".into(),
        }
    );

    // A Base implicit (tier 4).
    assert!(matches!(
        analysis.resolve_name(file, "println", after(src, "println"), Namespace::Value),
        Resolution::System { module, .. } if module == "Base"
    ));

    // An undefined name, queried at a valid in-body offset.
    assert_eq!(
        analysis.resolve_name(file, "nope", x, Namespace::Value),
        Resolution::Unresolved
    );
}

#[test]
fn visible_names_include_every_tier() {
    let mut db = IncrementalDatabase::new();
    db.set_library_packages(build_system_index(None));
    db.set_package_index("Greetings", package("Greetings", &["greet"]));

    let src = "using Greetings\nfunction f(a)\n    a\nend";
    let file = db.add_file(src);
    let analysis = db.snapshot();

    let offset = after(src, "    a");
    let names: Vec<String> = analysis
        .visible_names(file, offset, Namespace::Value)
        .into_iter()
        .map(|c| c.name.to_string())
        .collect();

    assert!(names.contains(&"a".to_string()), "parameter");
    assert!(names.contains(&"f".to_string()), "sibling function");
    assert!(names.contains(&"greet".to_string()), "using export");
    assert!(names.contains(&"println".to_string()), "Base implicit");
}
