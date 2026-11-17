//! Formatter fixtures.
//!
//! Two invariants over `tests/fixtures/formatter/<slug>/`:
//!
//! - **Gate** (`formatter_fixtures_match_expected`): every fixture that has a
//!   hand-authored `expected.jl` must format to it exactly
//!   (`format(input.jl) == expected.jl`). A fixture without `expected.jl` is not
//!   yet in the gate — its construct is still being authored. Presence of
//!   `expected.jl` *is* gate membership; there is no allowlist.
//! - **Stability** (`formatter_is_idempotent_and_stable`): over every fixture's
//!   `input.jl`, formatting is idempotent (`format(format(x)) == format(x)`) and
//!   its output parses cleanly (no parse diagnostics). This holds for *all*
//!   inputs, gated or not, so it guards against mangling any curated input as
//!   rules land.
//!
//! `expected.jl` is hand-authored under Tenet 1 (deterministic full reflow),
//! never captured from an external formatter. Grow the corpus with the
//! `formatter` skill.

use std::fs;
use std::path::{Path, PathBuf};

use fatou_formatter::verify::{ast_shape, verify_format};
use fatou_formatter::{FormatStyle, format, format_with_style};
use fatou_parser::parser::parse;

/// Fixtures whose shape the formatter is known to move, each with the reason.
///
/// `policy:` entries are recorded formatter behavior that is value- and
/// type-preserving but visible in the projection. `DEFECT:` entries are bugs
/// this test found; they stay listed only until they are fixed.
const KNOWN_DRIFT: &[(&str, &str)] = &[
    (
        "float_literals",
        "policy: float canonicalization (`.5` -> `0.5`, `007.50` -> `7.5`). The \
         projector renders a float's source spelling, so every row moves; each \
         rewrite here is value- and type-preserving",
    ),
    (
        "toplevel_semicolon",
        "policy: `a = 1; b = 2` splits onto separate lines, erasing the \
         `(toplevel-; ...)` grouping. Equivalent in a file; `;` only suppresses \
         output in the REPL, which fatou does not model",
    ),
    (
        "where_as_identifier",
        "policy: `where T` canonicalizes to `where {T}` (identical `Expr`)",
    ),
    (
        "where_bare_signature_break",
        "policy: `where T` canonicalizes to `where {T}` (identical `Expr`)",
    ),
    (
        "where_clauses",
        "policy: `where T` canonicalizes to `where {T}` (identical `Expr`), as \
         this fixture's own `expected.jl` asserts",
    ),
];

fn fixture_dirs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/formatter");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read formatter fixtures dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

fn slug(case: &Path) -> String {
    case.file_name().unwrap().to_string_lossy().to_string()
}

#[test]
fn short_circuit_guard_keeps_return_after_multiline_condition() {
    let input = "\
function check_header(lines)
    any(
        line -> occursin(HEADER_TAG, line),
        lines[1:min(end, HEADER_SCAN_LINES)],
    ) && return

    println(\"Header not found\")
end
";
    let style = FormatStyle {
        line_width: 50,
        ..FormatStyle::default()
    };
    let formatted = format_with_style(input, style).unwrap();
    assert_eq!(formatted, input);
    assert_eq!(format_with_style(&formatted, style).unwrap(), formatted);
    verify_format(input, &formatted).unwrap();

    let flat = "function check_header(lines); any(line -> occursin(HEADER_TAG, line), lines[1:min(end, HEADER_SCAN_LINES)]) && return; println(\"Header not found\"); end";
    assert_eq!(
        format_with_style(flat, style).unwrap(),
        input.replace("\n\n", "\n")
    );
}

#[test]
fn macro_newline_operators_preserve_the_program() {
    let expected = ast_shape("((@m x) - y)").unwrap();
    for input in ["(@m x\n - y)", "(@m x\r\n -y)", "((@m x) - y)"] {
        assert_eq!(ast_shape(input).as_ref(), Some(&expected), "{input:?}");
        let formatted = format(input).unwrap();
        assert_eq!(
            ast_shape(&formatted).as_ref(),
            Some(&expected),
            "{input:?}: {formatted:?}"
        );
        verify_format(input, &formatted).unwrap();
        assert_eq!(format(&formatted).unwrap(), formatted);
    }
    assert_eq!(
        verify_format("(@m x\n - y)", "(@m x - y)\n"),
        Err(fatou_formatter::verify::VerificationError::ChangedProgram),
    );
}

#[test]
fn macro_operator_fences_follow_expression_structure() {
    for (input, explicit) in [
        ("(@m\n - y)", "((@m) - y)"),
        ("(@m @n x\n - y)", "((@m @n x) - y)"),
        ("(-@m x\n - y)", "((-@m x) - y)"),
        ("(a + @m x\n - y)", "(a + (@m x) - y)"),
        ("(@m x\n == y)", "((@m x) == y)"),
        ("(@m x\n ? y : z)", "((@m x) ? y : z)"),
        ("(@m x\n where T)", "((@m x) where T)"),
        ("(@m x\n ::T)", "((@m x)::T)"),
        ("(@m x\n ...)", "((@m x)... )"),
        ("(@m [x]\n - y)", "(@m[x] - y)"),
        ("(@m(x)\n - y)", "(@m(x) - y)"),
        (
            "(@m very_long_function_name(first, second)\n - third)",
            "((@m very_long_function_name(first, second)) - third)",
        ),
    ] {
        assert_eq!(ast_shape(input), ast_shape(explicit), "{input:?}");
        for line_width in [24, 80] {
            let style = FormatStyle {
                line_width,
                ..FormatStyle::default()
            };
            let formatted = format_with_style(input, style).unwrap();
            verify_format(input, &formatted).unwrap();
            assert_eq!(
                format_with_style(explicit, style).unwrap(),
                formatted,
                "{input:?} at width {line_width}",
            );
            assert_eq!(format_with_style(&formatted, style).unwrap(), formatted);
        }
    }
}

#[test]
fn macro_generator_bodies_preserve_the_program() {
    for (input, explicit) in [
        ("(@m x\n for i in xs)", "((@m x) for i in xs)"),
        ("(@m x\r\n for i in xs)", "((@m x) for i in xs)\r\n"),
        ("(@m\n for i in xs)", "((@m) for i in xs)"),
        ("(@m @n x\n for i in xs)", "((@m @n x) for i in xs)"),
        ("(@m [x]\n for i in xs)", "(@m[x] for i in xs)"),
    ] {
        assert_eq!(ast_shape(input), ast_shape(explicit), "{input:?}");
        for line_width in [24, 80] {
            let style = FormatStyle {
                line_width,
                ..FormatStyle::default()
            };
            let formatted = format_with_style(input, style).unwrap();
            verify_format(input, &formatted).unwrap();
            assert_eq!(format_with_style(explicit, style).unwrap(), formatted);
            assert_eq!(format_with_style(&formatted, style).unwrap(), formatted);
        }
    }
}

#[test]
fn macro_list_items_keep_commas_outside_the_call() {
    for (input, explicit) in [
        ("(@m a\n, b)", "((@m a), b)"),
        ("[@m a\n, b]", "[(@m a), b]"),
        ("f(@m a\n, b)", "f((@m a), b)"),
        ("(@m a\n,)", "((@m a),)"),
        ("(@m a\n, @n b\n, c)", "((@m a), (@n b), c)"),
    ] {
        assert_eq!(ast_shape(input), ast_shape(explicit), "{input:?}");
        for line_width in [24, 80] {
            let style = FormatStyle {
                line_width,
                ..FormatStyle::default()
            };
            let formatted = format_with_style(input, style).unwrap();
            verify_format(input, &formatted).unwrap();
            assert_eq!(format_with_style(explicit, style).unwrap(), formatted);
            assert_eq!(format_with_style(&formatted, style).unwrap(), formatted);
        }
    }
}

/// Gate: every fixture with an `expected.jl` must format to it exactly.
#[test]
fn formatter_fixtures_match_expected() {
    let mut failures = Vec::new();
    for case in fixture_dirs() {
        let expected_path = case.join("expected.jl");
        if !expected_path.is_file() {
            continue; // not yet in the gate
        }
        let name = slug(&case);
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");
        let expected = fs::read_to_string(&expected_path).expect("read expected.jl");

        match format(&input) {
            Ok(formatted) if formatted == expected => {}
            Ok(_) => failures.push(name),
            Err(_) => failures.push(format!("{name} (format error)")),
        }
    }
    assert!(
        failures.is_empty(),
        "formatter fixtures diverge from expected.jl: {failures:?}"
    );
}

/// Stability: formatting is idempotent and its output parses cleanly, over every
/// `input.jl` (gated or not).
#[test]
fn formatter_is_idempotent_and_stable() {
    for case in fixture_dirs() {
        let name = slug(&case);
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");

        let once = format(&input).expect("format input");
        let twice = format(&once).expect("format formatted");
        assert_eq!(twice, once, "format is not idempotent for `{name}`");

        let reparsed = parse(&once);
        assert!(
            reparsed.diagnostics.is_empty(),
            "formatted output of `{name}` does not parse cleanly: {:?}",
            reparsed.diagnostics
        );
    }
}

/// AST preservation: formatting moves layout, never the program.
///
/// Idempotence and clean reparse are both local — neither says the output still
/// *means* what the input meant. This compares
/// [`fatou_formatter::verify::ast_shape`] across the format, over every
/// `input.jl` (gated or not), so a construct that silently changes meaning fails
/// here even while its `expected.jl` passes.
///
/// `KNOWN_DRIFT` is the exemption list, and it is not an allowlist to grow into:
/// each entry is either a recorded formatter policy that moves the projection or
/// a defect this test found. Entries are checked for staleness — an entry fails
/// until it is removed once its slug stops drifting, and equally once its slug
/// stops naming a fixture — so neither a fixed bug nor a renamed fixture can
/// leave its exemption behind.
#[test]
fn formatter_preserves_ast_shape() {
    let mut drifted = Vec::new();
    let mut stale = Vec::new();
    let mut exercised = Vec::new();

    for case in fixture_dirs() {
        let name = slug(&case);
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");
        let known = KNOWN_DRIFT.iter().find(|(s, _)| *s == name);

        let Some(before) = ast_shape(&input) else {
            assert!(
                known.is_none(),
                "`{name}` is listed in KNOWN_DRIFT but has no comparable shape"
            );
            continue; // out of domain: does not parse cleanly, or projects a sentinel
        };
        exercised.push(name.clone());
        let formatted = format(&input).expect("format input");
        verify_format(&input, &formatted)
            .unwrap_or_else(|error| panic!("safe formatting rejected `{name}`: {error}"));
        // The input projected, so a formatted text with no shape is the
        // formatter's doing — but `ast_shape` folds a failed parse and a
        // projector sentinel into `None`, and they point at different code.
        let after = ast_shape(&formatted).unwrap_or_else(|| {
            let diagnostics = parse(&formatted).diagnostics;
            assert!(
                !diagnostics.is_empty(),
                "formatted output of `{name}` parses but no longer projects — the \
                 formatter produced a construct the projector cannot render"
            );
            panic!("formatted output of `{name}` no longer parses cleanly: {diagnostics:?}")
        });

        match (before == after, known) {
            (false, None) => drifted.push(format!(
                "{name}\n     before: {before}\n     after:  {after}"
            )),
            (true, Some((_, why))) => {
                stale.push(format!("{name} (no longer drifts; listed as: {why})"))
            }
            _ => {}
        }
    }

    // An entry no fixture reached is as stale as one that stopped drifting: the
    // fixture was renamed or deleted, and the exemption now hides nothing.
    stale.extend(
        KNOWN_DRIFT
            .iter()
            .filter(|(s, _)| !exercised.iter().any(|name| name == s))
            .map(|(s, why)| format!("{s} (matches no fixture; listed as: {why})")),
    );

    assert!(
        stale.is_empty(),
        "KNOWN_DRIFT entries no longer apply — remove them: {stale:?}"
    );
    assert!(
        drifted.is_empty(),
        "formatting changed the program shape for:\n  - {}",
        drifted.join("\n  - ")
    );
}
