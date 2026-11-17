use fatou_parser::parser::{
    Edit, ParseOptions, ReparseBase, SyntaxEdition, fingerprint, parse, parse_with_options,
    reparse_edits_with_options, reparse_with_options,
};

fn options(edition: SyntaxEdition) -> ParseOptions {
    ParseOptions {
        syntax_edition: edition,
    }
}

#[test]
fn editions_change_grammar_without_losing_source() {
    for newline in ["\n", "\r\n"] {
        for (source, old_valid, new_valid) in [
            ("typegroup = 1", true, false),
            ("typegroup(x) = x", true, false),
            ("@m typegroup", true, false),
            ("typegroup\nstruct A end\nend", false, true),
            ("@m typegroup \"text\"", false, false),
            ("@m typegroup \"text\" end", false, true),
            ("@m typegroup\n\"text\"", true, false),
            ("a[return end]", false, true),
            ("a[return begin, end]", false, true),
            ("x ? return : y", false, true),
            ("(@info \"message\"\n x=1)", false, true),
            ("break outer 1", false, true),
            ("for x in xs; [break]; end", false, true),
            (
                "for x in xs; if x; break else continue end; end",
                false,
                true,
            ),
            ("for x in xs; f(break); end", true, true),
            ("f(; typegroup=1)", true, false),
            ("f(typegroup=1)", true, false),
            ("(; typegroup=1)", true, false),
            ("f(; var\"typegroup\"=1)", true, true),
            ("x +%= 1", false, true),
            ("primitive type Bits 7 end", true, true),
            ("public foo", true, true),
        ] {
            let source = source.replace('\n', newline);
            for edition in SyntaxEdition::ALL {
                let parsed = parse_with_options(&source, options(edition));
                let valid = if edition == SyntaxEdition::Julia113 {
                    old_valid
                } else {
                    new_valid
                };
                assert_eq!(
                    parsed.diagnostics.is_empty(),
                    valid,
                    "{edition}: {source:?}: {:?}",
                    parsed.diagnostics
                );
                assert_eq!(parsed.cst.text().to_string(), source);
            }
        }
    }
}

#[test]
fn typegroup_is_a_reserved_declaration_name_in_new_editions() {
    for source in [
        "function typegroup(x) end end",
        "macro typegroup(x) end end",
        "struct typegroup end end",
        "module typegroup end end",
        "abstract type typegroup end end",
        "primitive type typegroup 8 end",
    ] {
        for edition in [SyntaxEdition::Julia114, SyntaxEdition::Julia115] {
            let parsed = parse_with_options(source, options(edition));
            assert_eq!(parsed.cst.text().to_string(), source);
            assert!(
                parsed.diagnostics.iter().any(|diag| {
                    diag.message.contains("reserved keyword used as a name")
                        && &source[diag.start..diag.end] == "typegroup"
                }),
                "{edition}: {source:?}: {:?}",
                parsed.diagnostics
            );
        }
    }
}

#[test]
fn old_loop_exits_leave_the_delimiter_for_the_enclosing_expression() {
    use fatou_parser::parser::to_juliasyntax_sexpr_with_options;

    let opts = options(SyntaxEdition::Julia113);
    for keyword in ["break", "continue"] {
        let source = format!("for x in xs; f({keyword} #= comment =#, x); end");
        let parsed = parse_with_options(&source, opts);
        assert_eq!(parsed.cst.text().to_string(), source);
        assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
        assert_eq!(
            to_juliasyntax_sexpr_with_options(&parsed.cst, &parsed.diagnostics, opts),
            format!(
                "(toplevel (for (iteration (in x xs)) (block (call f ({keyword} (error-t)) x))))"
            )
        );
    }
}

#[test]
fn legacy_parse_uses_latest_edition() {
    let source = "(@m a\n b)";
    let legacy = parse(source);
    let explicit = parse_with_options(source, ParseOptions::default());
    assert_eq!(fingerprint(&legacy.cst), fingerprint(&explicit.cst));
    assert_eq!(legacy.diagnostics, explicit.diagnostics);
}

#[test]
fn reparsing_keeps_the_edition_and_rejects_edition_changes() {
    for edition in SyntaxEdition::ALL {
        let opts = options(edition);
        let old = "abc = 1\nx = 2\ny = 3\n";
        let new = "abd = 1\nx = 2\ny = 3\n";
        let parsed = parse_with_options(old, opts);
        let green = parsed.cst.green().to_owned();
        let base = ReparseBase {
            text: old,
            green: &green,
            diagnostics: &parsed.diagnostics,
            options: opts,
        };
        let edit = Edit {
            range: 0..3,
            insert: "abd".into(),
        };
        let full = parse_with_options(new, opts);
        for result in [
            reparse_with_options(base, &edit, new, opts),
            reparse_edits_with_options(base, &[edit], new, opts),
        ] {
            {
                let result = result.expect("same-edition identifier edits use a reparse tier");
                assert_eq!(
                    fingerprint(&fatou_parser::syntax::SyntaxNode::new_root(result.green)),
                    fingerprint(&full.cst)
                );
                assert_eq!(result.diagnostics, full.diagnostics);
            }
        }
        let other = options(if edition == SyntaxEdition::Julia113 {
            SyntaxEdition::Julia114
        } else {
            SyntaxEdition::Julia113
        });
        assert!(
            reparse_with_options(
                base,
                &Edit {
                    range: 0..0,
                    insert: String::new()
                },
                old,
                other
            )
            .is_none()
        );
    }
}

#[test]
fn version_normalization_matches_the_locked_julia() {
    assert_eq!(SyntaxEdition::normalized(1, 6), SyntaxEdition::Julia113);
    assert_eq!(SyntaxEdition::normalized(1, 14), SyntaxEdition::Julia114);
    assert_eq!(SyntaxEdition::normalized(1, 99), SyntaxEdition::Julia115);
    assert_eq!(SyntaxEdition::normalized(2, 0), SyntaxEdition::Julia115);
    assert!("1.14".parse::<SyntaxEdition>().is_ok());
    assert!("1.99".parse::<SyntaxEdition>().is_err());
}
