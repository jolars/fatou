//! Label names belong to control flow, while break values use ordinary scope
//! and binding resolution.

use fatou::parser::parse;
use fatou::semantic::SemanticModel;

fn model(src: &str) -> SemanticModel {
    let parsed = parse(src);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    SemanticModel::build(&parsed.cst)
}

#[test]
fn control_labels_are_not_variable_reads() {
    for src in [
        "@label done\n@goto done\n",
        "@label(done)\n@goto(done)\n",
        "Base.@label(done)\nBase.@goto(done)\n",
        "@Base.label((done))\n@Base.goto((done))\n",
        "@label outer while ready\n    continue outer\n    break outer value\nend\n",
        "@label(outer, begin\n    break outer value\nend)\n",
        "Base.@label outer begin\n    break outer value\nend\n",
        "@label begin\n    break _ value\nend\n",
        "@label var\"outer block\" begin\n    break var\"outer block\" value\nend\n",
    ] {
        let m = model(src);
        let reads: Vec<_> = m.free_reads().map(|read| read.name.as_str()).collect();
        assert!(
            reads
                .iter()
                .all(|name| matches!(*name, "ready" | "value" | "Base")),
            "control label read as a variable in {src:?}: {reads:?}"
        );
        assert!(m.bindings().is_empty(), "labels must not bind variables");
        if src.contains("value") {
            assert!(
                reads.contains(&"value"),
                "break value was not read: {src:?}"
            );
        }
    }
}

#[test]
fn labels_do_not_mark_same_named_variables_as_read() {
    for src in [
        "function f(value)\n    target = 1\n    @label target begin\n        break target value\n    end\nend\n",
        "function f(value)\n    target = 1\n    @goto target\n    @label target\n    value\nend\n",
    ] {
        let m = model(src);
        let target = m.bindings().iter().find(|b| b.name == "target").unwrap();
        assert!(!target.read);
        let value = m.bindings().iter().find(|b| b.name == "value").unwrap();
        assert!(value.read);
    }
}

#[test]
fn labeled_block_bodies_declare_assignments_in_the_enclosing_scope() {
    // An @label body is an ordinary expression, including a direct assignment
    // that would be treated as a keyword argument of an unknown macro.
    for src in [
        "@label named x = value\nuse(x)\n",
        "@label x = value\nuse(x)\n",
        "@label(named, x = value)\nuse(x)\n",
        "@label(x = value)\nuse(x)\n",
    ] {
        let m = model(src);
        let x = m.bindings().iter().find(|b| b.name == "x");
        assert!(
            x.is_some_and(|b| b.read),
            "body assignment was not bound: {src:?}"
        );
        assert_eq!(m.scopes().len(), 1, "a label does not add a variable scope");
    }
}

#[test]
fn break_values_keep_assignments_and_closure_captures() {
    let src = "function f(value)\n    @label done begin\n        break done (saved = value; () -> saved)\n    end\nend\n";
    let m = model(src);
    assert!(m.free_reads().next().is_none());
    for name in ["saved", "value"] {
        assert!(
            m.bindings().iter().any(|b| b.name == name && b.read),
            "{name}"
        );
    }
}

#[test]
fn unrelated_macros_and_label_interpolations_keep_their_reads() {
    let m = model("Other.@label name value\nOther.@goto name\n:(break $target $value)\n");
    let reads: Vec<_> = m.free_reads().map(|read| read.name.as_str()).collect();
    assert_eq!(
        reads,
        ["Other", "name", "value", "Other", "name", "target", "value"]
    );
}

#[test]
fn parenthesized_break_values_assign_without_changing_named_tuple_fields() {
    let m = model(
        "function f(value)\n    @label done begin\n        break done (x = value; y = x; (; field = y))\n    end\nend\n",
    );
    assert!(m.free_reads().next().is_none());
    for name in ["value", "x", "y"] {
        assert!(
            m.bindings().iter().any(|b| b.name == name && b.read),
            "{name}"
        );
    }
    assert!(!m.bindings().iter().any(|b| b.name == "field"));
}
