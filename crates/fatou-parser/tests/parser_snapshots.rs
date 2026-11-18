//! Snapshot the CST + diagnostics for each parser fixture, and assert the
//! losslessness invariant (`reconstruct(input) == input`) on every case.

use std::fs;
use std::path::{Path, PathBuf};

use fatou_parser::parser::{ParseOptions, parse, parse_with_options, reconstruct};

fn fixture_cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parser");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read parser fixtures dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

#[test]
fn parser_fixtures() {
    for case in fixture_cases() {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");

        let options = fs::read_to_string(case.join("edition.txt"))
            .map(|v| ParseOptions {
                syntax_edition: v.trim().parse().unwrap(),
            })
            .unwrap_or_default();
        let output = parse_with_options(&input, options);
        assert_eq!(
            output.cst.text().to_string(),
            input,
            "losslessness failed for `{name}`"
        );
        let mut rendered = format!("{:#?}", output.cst);
        // Rust's Unicode tables change which new characters Debug escapes.
        // Normalize these fixtures so snapshots agree across toolchains.
        if name == "unicode_18_identifiers" {
            for ch in input.chars().filter(|ch| !ch.is_ascii()) {
                rendered = rendered.replace(ch, &format!("\\u{{{:x}}}", ch as u32));
            }
        }
        if name == "unicode_18_operator_suffixes" {
            for ch in ['\u{209d}', '\u{209e}', '\u{209f}', '\u{1dfd0}'] {
                rendered = rendered.replace(ch, &format!("\\u{{{:x}}}", ch as u32));
            }
        }
        for diag in &output.diagnostics {
            rendered.push_str(&format!(
                "\ndiagnostic [{}..{}]: {}",
                diag.start, diag.end, diag.message
            ));
        }
        insta::assert_snapshot!(name, rendered);
    }
}

#[test]
fn standalone_colon_accepts_trailing_whitespace() {
    let input = include_str!("fixtures/parser/colon_value_whitespace/input.jl");
    let output = parse(input);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

    for input in [": ", ":\t", ": #= comment =#", ": # comment"] {
        let output = parse(input);
        assert!(
            output.diagnostics.is_empty(),
            "{input:?}: {:?}",
            output.diagnostics
        );
        assert_eq!(reconstruct(input), input);
    }
}

#[test]
fn return_operands_keep_index_and_ternary_context() {
    let input = include_str!("fixtures/parser/return_context/input.jl");
    for newline in ["\n", "\r\n"] {
        let input = input.replace('\n', newline);
        let output = parse(&input);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(reconstruct(&input), input);
    }
}

#[test]
fn unicode_18_fixtures_match_the_oracle() {
    use fatou_parser::parser::{normalize_sexpr, to_juliasyntax_sexpr};

    for (input, expected) in [
        (
            include_str!("fixtures/parser/unicode_18_identifiers/input.jl"),
            include_str!("fixtures/oracle/unicode_18_identifiers/expected.sexpr"),
        ),
        (
            include_str!("fixtures/parser/unicode_18_operator_suffixes/input.jl"),
            include_str!("fixtures/oracle/unicode_18_operator_suffixes/expected.sexpr"),
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            let input = input.replace('\n', newline);
            let output = parse(&input);
            assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
            assert_eq!(reconstruct(&input), input);
            assert_eq!(
                normalize_sexpr(&to_juliasyntax_sexpr(&output.cst, &output.diagnostics)),
                normalize_sexpr(expected),
            );
        }
    }
}

#[test]
fn return_context_keeps_newline_and_declaration_boundaries() {
    for input in [
        "[return end]",
        "(return end)",
        "f(return end)",
        "a[return\nend]",
        "a[return # comment\nend]",
        "x ? return\n: y",
        "x ? return a\n: y",
        "x ? return :a : y",
        "a[return begin x end]",
        "a[return global x = end]",
        "a[global x = end]",
        "x ? global x = y : z",
    ] {
        for newline in ["\n", "\r\n"] {
            let input = input.replace('\n', newline);
            let output = parse(&input);
            assert!(!output.diagnostics.is_empty(), "{input:?}");
            assert_eq!(reconstruct(&input), input);
        }
    }
}

#[test]
fn quote_colon_keeps_whitespace_diagnostics() {
    for input in [
        ": name",
        "A[: name]",
        "f(: name)",
        ": (name)",
        ": +",
        "A[: end]",
        "A.: end",
        "A[:\n]",
        "[: # comment\n]",
        ":\n",
    ] {
        let output = parse(input);
        let anchor = input.find(':').unwrap() + 1;
        assert!(
            output.diagnostics.iter().any(|diagnostic| {
                diagnostic.message == "whitespace after `:`"
                    && diagnostic.start == anchor
                    && diagnostic.end == anchor
            }),
            "{input:?}: {:?}",
            output.diagnostics,
        );
        assert_eq!(reconstruct(input), input);
    }
}

#[test]
fn radical_juxtaposition_parses_cleanly() {
    let input = include_str!("fixtures/parser/radical_juxtaposition/input.jl");
    let output = parse(input);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
}

#[test]
fn dotted_wrapping_arithmetic_operators_parse_cleanly() {
    let input = include_str!("fixtures/parser/dotted_wrapping_arithmetic_operators/input.jl");
    let output = parse(input);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

    // The released corpus predates these operators, so this snapshot is checked
    // against the pinned development oracle in an isolated corpus.
    let projected = fatou_parser::parser::to_juliasyntax_sexpr(&output.cst, &output.diagnostics);
    insta::assert_snapshot!("dotted_wrapping_arithmetic_operators_sexpr", projected);
}

#[test]
fn parenthesized_macro_loop_arguments_parse_cleanly() {
    let input = include_str!("fixtures/parser/parenthesized_macro_loop_argument/input.jl");
    assert!(parse(input).diagnostics.is_empty());
}

#[test]
fn macro_arguments_stop_before_newline_operators() {
    use fatou_parser::parser::to_juliasyntax_sexpr;

    for newline in [
        "\n",
        "\r\n",
        " # comment\n",
        " #= comment =#\n",
        "\n #= comment =#",
        "\n # comment\n",
    ] {
        for (tail, expected) in [
            (
                "- y",
                "(toplevel (call-i (macrocall (macro_name m) x) - y))",
            ),
            ("-y", "(toplevel (call-i (macrocall (macro_name m) x) - y))"),
            (
                "+ y",
                "(toplevel (call-i (macrocall (macro_name m) x) + y))",
            ),
            (
                "in ys",
                "(toplevel (call-i (macrocall (macro_name m) x) in ys))",
            ),
            (
                "where T",
                "(toplevel (where (macrocall (macro_name m) x) T))",
            ),
            ("::T", "(toplevel (::-i (macrocall (macro_name m) x) T))"),
        ] {
            let input = format!("(@m x{newline} {tail})");
            let output = parse(&input);
            assert!(
                output.diagnostics.is_empty(),
                "{input:?}: {:?}",
                output.diagnostics
            );
            assert_eq!(
                to_juliasyntax_sexpr(&output.cst, &output.diagnostics),
                expected,
                "{input:?}",
            );
            assert_eq!(reconstruct(&input), input);
        }
    }
}

#[test]
fn macro_newline_operator_fixture_parses_cleanly() {
    let input = include_str!("fixtures/parser/macro_newline_operator/input.jl");
    let output = parse(input);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
}

#[test]
fn parenthesized_multiline_macro_arguments_parse_cleanly() {
    let input = include_str!("fixtures/parser/parenthesized_multiline_macro/input.jl");
    for input in [input.to_owned(), input.replace('\n', "\r\n")] {
        let output = parse(&input);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(reconstruct(&input), input);
    }
}

#[test]
fn multiline_macro_arguments_belong_to_the_immediate_parenthesized_call() {
    use fatou_parser::parser::to_juliasyntax_sexpr;

    for gap in ["\n", "\r\n", " # comment\n", "\n # comment\n", "\n\n"] {
        for (source, expected) in [
            ("(@m a{gap} b)", "(macrocall (macro_name m) a b)"),
            ("(A.@m a{gap} b)", "(macrocall (. A (macro_name m)) a b)"),
            (
                "(@m @n a{gap} b)",
                "(macrocall (macro_name m) (macrocall (macro_name n) a) b)",
            ),
            (
                "(@m (@n a{gap} b){gap} c)",
                "(macrocall (macro_name m) (macrocall (macro_name n) a b) c)",
            ),
            (
                "(@m a{gap} b, c)",
                "(macrocall (macro_name m) a (tuple b c))",
            ),
            (
                "(@m a{gap} b, c; d)",
                "(block-p (macrocall (macro_name m) a (tuple b c)) d)",
            ),
            ("(@m a{gap}, b)", "(tuple-p (macrocall (macro_name m) a) b)"),
            (
                "(@m a{gap} for i in xs)",
                "(generator (macrocall (macro_name m) a) (iteration (in i xs)))",
            ),
        ] {
            let input = source.replace("{gap}", gap);
            let output = parse(&input);
            assert!(
                output.diagnostics.is_empty(),
                "{input:?}: {:?}",
                output.diagnostics
            );
            assert_eq!(
                to_juliasyntax_sexpr(&output.cst, &output.diagnostics),
                format!("(toplevel {expected})"),
                "{input:?}",
            );
            assert_eq!(reconstruct(&input), input);
        }
    }
}

#[test]
fn declarations_continue_after_newlines() {
    let input = include_str!("fixtures/parser/declaration_newline_continuation/input.jl");
    assert!(parse(input).diagnostics.is_empty());
}

#[test]
fn using_base_aliases_report_parse_errors() {
    let output = parse("using A as B\nusing A, B as C\n");

    assert_eq!(
        output
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.start,
                    diagnostic.end,
                    diagnostic.message.as_str(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (8, 10, "invalid `as` alias"),
            (24, 26, "invalid `as` alias"),
        ]
    );
}

#[test]
fn break_continue_commas_report_one_diagnostic_per_keyword() {
    for keyword in ["break", "continue"] {
        for (prefix, suffix) in [
            ("", ", y"),
            ("(", ", y)"),
            ("f(", ", y)"),
            ("[", ", y]"),
            ("{", ", y}"),
            ("A[", ", y]"),
            (":(", ", y)"),
        ] {
            let input = format!("{prefix}{keyword}{suffix}");
            let output = parse(&input);
            let anchor = prefix.len() + keyword.len();
            assert_eq!(
                output
                    .diagnostics
                    .iter()
                    .map(|d| (d.start, d.end))
                    .collect::<Vec<_>>(),
                vec![(anchor, anchor)],
                "{input}",
            );
        }
    }
}

#[test]
fn labeled_keyword_commas_form_tuples() {
    for keyword in ["break", "continue"] {
        for (input, expected) in [
            (
                format!("{keyword} lbl, y"),
                format!("(toplevel (tuple ({keyword} lbl) y))"),
            ),
            (
                format!("{keyword} lbl, y = values"),
                format!("(toplevel (= (tuple ({keyword} lbl) y) values))"),
            ),
            (
                format!("f({keyword} lbl, y)"),
                format!("(toplevel (call f ({keyword} lbl) y))"),
            ),
            (
                format!("[{keyword} lbl, y]"),
                format!("(toplevel (vect ({keyword} lbl) y))"),
            ),
        ] {
            let output = parse(&input);
            assert!(
                output.diagnostics.is_empty(),
                "{input}: {:?}",
                output.diagnostics
            );
            assert_eq!(
                fatou_parser::parser::to_juliasyntax_sexpr(&output.cst, &output.diagnostics),
                expected,
                "{input}",
            );
            assert_eq!(reconstruct(&input), input);
        }
    }
}

#[test]
fn labeled_keyword_recovery_records_eof() {
    for (input, expected) in [
        (
            "break l x y",
            "(toplevel (break l x) (error-t y (error-t)))",
        ),
        (
            "continue l x y",
            "(toplevel (continue l) (error-t x y (error-t)))",
        ),
        (
            "break l x y # comment",
            "(toplevel (break l x) (error-t y (error-t)))",
        ),
        (
            "continue l x #= comment =#",
            "(toplevel (continue l) (error-t x (error-t)))",
        ),
    ] {
        let output = parse(input);
        assert_eq!(
            fatou_parser::parser::to_juliasyntax_sexpr(&output.cst, &output.diagnostics),
            expected,
            "{input}",
        );
        let diagnostics = &output.diagnostics;
        assert_eq!(diagnostics.len(), 2, "{input}: {diagnostics:?}");
        assert_eq!(
            diagnostics[0].message,
            format!(
                "unexpected token after `{}`",
                input.split_whitespace().next().unwrap()
            ),
            "{input}",
        );
        assert_eq!(diagnostics[1].message, "premature end of input", "{input}");
        assert_eq!(
            (diagnostics[1].start, diagnostics[1].end),
            (input.len(), input.len())
        );
        assert_eq!(reconstruct(input), input);
    }
}

#[test]
fn labeled_keyword_recovery_stops_at_delimiters() {
    for (input, expected) in [
        (
            "break l x y\nafter",
            "(toplevel (break l x) (error-t y) after)",
        ),
        (
            "continue l x\nafter",
            "(toplevel (continue l) (error-t x) after)",
        ),
        (
            "break l x y\r\nafter",
            "(toplevel (break l x) (error-t y) after)",
        ),
        (
            "continue l x # comment\nafter",
            "(toplevel (continue l) (error-t x) after)",
        ),
        (
            "break l x y; after",
            "(toplevel (toplevel-; (break l x) (error-t y) after))",
        ),
        (
            "f(continue l x, y)",
            "(toplevel (call f (continue l) (error-t x) y))",
        ),
        (
            "begin continue l x\nafter end",
            "(toplevel (block (continue l) (error-t x) after))",
        ),
        (
            "x ? continue l z : y",
            "(toplevel (? x (continue l) (error-t z) y))",
        ),
    ] {
        let output = parse(input);
        assert_eq!(
            fatou_parser::parser::to_juliasyntax_sexpr(&output.cst, &output.diagnostics),
            expected,
            "{input}",
        );
        assert_eq!(
            output.diagnostics.len(),
            1,
            "{input}: {:?}",
            output.diagnostics
        );
        assert_eq!(reconstruct(input), input);
    }
}

#[test]
fn labeled_keyword_recovery_projection() {
    let input = include_str!("fixtures/parser/labeled_keyword_recovery/input.jl");
    let output = parse(input);

    // The released corpus predates labels, so this snapshot is checked against
    // the pinned development oracle in an isolated corpus.
    let projected = fatou_parser::parser::to_juliasyntax_sexpr(&output.cst, &output.diagnostics);
    insta::assert_snapshot!("labeled_keyword_recovery_sexpr", projected);
}

#[test]
fn labeled_keyword_break_values_keep_boundaries() {
    for (input, expected) in [
        ("break l x, y", "(toplevel (break l (tuple x y)))"),
        ("x ? break l x : y", "(toplevel (? x (break l x) y))"),
        (
            "break l x, y z",
            "(toplevel (break l (tuple x y)) (error-t z (error-t)))",
        ),
    ] {
        let output = parse(input);
        assert_eq!(
            fatou_parser::parser::to_juliasyntax_sexpr(&output.cst, &output.diagnostics),
            expected,
            "{input}",
        );
        assert_eq!(reconstruct(input), input);
    }
}

#[test]
fn bare_return_leaves_the_array_separator() {
    use fatou_parser::parser::SyntaxEdition;
    use fatou_parser::parser::to_juliasyntax_sexpr;

    let input = "f() = [return, 1]";
    for syntax_edition in [
        SyntaxEdition::Julia113,
        SyntaxEdition::Julia114,
        SyntaxEdition::Julia115,
    ] {
        let parsed = parse_with_options(input, ParseOptions { syntax_edition });
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert!(
            to_juliasyntax_sexpr(&parsed.cst, &parsed.diagnostics).contains("(vect (return) 1)")
        );
    }
}
