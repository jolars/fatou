use fatou_parser::documentation::ast::{
    Admonition, Block, CodeBlock, Document, DocumenterDirective, DocumenterLinkKind, FenceKind,
    Heading, Inline, InlineCode, Link, List,
};
use fatou_parser::documentation::syntax::SyntaxKind;
use fatou_parser::documentation::{DiagnosticKind, parse, reconstruct};
use fatou_parser::parser::parse as parse_julia;
use fatou_parser::{ast::DocAttachment, ast::DocText};
use rowan::ast::AstNode as _;

#[test]
fn documentation_parse_is_lossless_for_every_byte() {
    let input = concat!(
        "# Heading\r\n",
        "\r\n",
        "> quoted *text*\r\n",
        "\r\n",
        "!!! note \"Mind the gap\"\r\n",
        "    Body with [`f`](@ref f).\r\n",
        "\r\n",
        "| α | β |\r\n",
        "| :- | -: |\r\n",
        "| 1 | 2 |\r\n",
    );
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
}

#[test]
fn documentation_parse_is_lossless_at_utf8_boundaries() {
    for input in [
        "α [`β`](@ref Γ.δ) -- $x$\n!!! note \"λ\"\n    `μ` and $(f(ν))\n",
        "- [α\r\n  β](@ref\r\n  Γ.δ) **λ\r\n  μ** `ν\r\n  ξ`\r\n",
        "> [α\n> β](@ref)\n> `λ\n> μ`\n",
        "!!! note\n    ```@example α\n    β = 1\n\n\n\tγ(β)\n    ```\n",
    ] {
        let boundaries: Vec<_> = input
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(input.len()))
            .collect();

        for (start_index, &start) in boundaries.iter().enumerate() {
            for &end in &boundaries[start_index..] {
                let source = &input[start..end];
                assert_eq!(reconstruct(&parse(source).cst), source);
            }
        }
    }
}

#[test]
fn wrapped_inlines_preserve_utf8_ranges_and_container_prefixes() {
    let body = concat!(
        "Intro\n",
        "[API α\nReference β](@ref Main.δ) with **bold α\ntext β**, ",
        "*italic α\ntext β*, and `code α\ntext β`.",
    );
    for (header, first_prefix, continuation) in [
        ("", "", ""),
        ("", "- ", "  "),
        ("", "1. ", "   "),
        ("!!! note\n", "    ", "    "),
        ("", "> ", "> "),
        ("", "> - ", ">   "),
        ("", "[^note]: ", "    "),
    ] {
        for newline in ["\n", "\r\n"] {
            for trailing_newline in [false, true] {
                let mut input = format!(
                    "{header}{first_prefix}{}",
                    body.replace('\n', &format!("\n{continuation}"))
                );
                if trailing_newline {
                    input.push('\n');
                }
                let input = input.replace('\n', newline);
                let parsed = parse(&input);
                assert!(
                    parsed.diagnostics.is_empty(),
                    "{input:?}: {:?}",
                    parsed.diagnostics
                );
                assert_eq!(reconstruct(&parsed.cst), input);
                let link = parsed
                    .cst
                    .descendants()
                    .find_map(Link::cast)
                    .expect("wrapped link");
                let label = link
                    .label()
                    .map(|inline| match inline {
                        Inline::Text(token) => token.text().to_string(),
                        Inline::SoftBreak(token) => {
                            assert_eq!(token.text(), newline);
                            " ".to_string()
                        }
                        other => panic!("unexpected label inline: {other:?}"),
                    })
                    .collect::<String>();
                assert_eq!(label, "API α Reference β");
                assert_eq!(link.destination(), "@ref Main.δ");
                let link_range = link.syntax().text_range();
                assert_eq!(usize::from(link_range.start()), input.find("[API").unwrap());
                assert_eq!(
                    &input[usize::from(link_range.start())..usize::from(link_range.end())],
                    format!("[API α{newline}{continuation}Reference β](@ref Main.δ)")
                );
                let destination = link.destination_range().unwrap();
                assert_eq!(
                    &input[usize::from(destination.start())..usize::from(destination.end())],
                    "@ref Main.δ"
                );

                let code = parsed
                    .cst
                    .descendants()
                    .find_map(InlineCode::cast)
                    .expect("wrapped inline code");
                assert_eq!(code.content(), "code α text β");
                for kind in [SyntaxKind::STRONG, SyntaxKind::EMPHASIS] {
                    let node = parsed
                        .cst
                        .descendants()
                        .find(|node| node.kind() == kind)
                        .expect("wrapped emphasis");
                    assert!(
                        node.children_with_tokens()
                            .any(|element| element.kind() == SyntaxKind::SOFT_BREAK)
                    );
                }
                for token in parsed
                    .cst
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    let range = token.text_range();
                    assert_eq!(
                        &input[usize::from(range.start())..usize::from(range.end())],
                        token.text()
                    );
                }
            }
        }
    }
}

#[test]
fn inline_delimiters_do_not_cross_block_boundaries() {
    for (open, close, kind) in [
        ("[open", "close](@ref)", SyntaxKind::LINK),
        ("**open", "close**", SyntaxKind::STRONG),
        ("`open", "close`", SyntaxKind::INLINE_CODE),
    ] {
        let cases = [
            format!("{open}\n\n{close}\n"),
            format!("{open}\n# Heading\n{close}\n"),
            format!("{open}\n> quoted\n{close}\n"),
            format!("{open}\n- item\n{close}\n"),
            format!("{open}\n!!! note\n    body\n{close}\n"),
            format!("{open}\n```julia\nx = 1\n```\n{close}\n"),
            format!("- {open}\n- {close}\n"),
            format!("- {open}\n\n  {close}\n"),
            format!("- {open}\n  - nested\n  {close}\n"),
            format!("- {open}\n  ```julia\n  x = 1\n  ```\n  {close}\n"),
            format!("- {open}\n\n      x = 1\n\n  {close}\n"),
            format!("!!! note\n    {open}\n\n    {close}\n"),
            format!("!!! note\n    {open}\n    - item\n    {close}\n"),
            format!("!!! note\n    {open}\n{close}\n"),
            format!("> {open}\n>\n> {close}\n"),
            format!("> {open}\n> - item\n> {close}\n"),
            format!("> - {open}\n> - {close}\n"),
            format!("[^note]: {open}\n\n    {close}\n"),
        ];
        for input in cases {
            for input in [input.clone(), input.replace('\n', "\r\n")] {
                let parsed = parse(&input);
                assert_eq!(reconstruct(&parsed.cst), input);
                assert!(
                    !parsed.cst.descendants().any(|node| node.kind() == kind),
                    "{input:?}\n{:#?}",
                    parsed.cst
                );
            }
        }
    }
}

#[test]
fn wrapped_inline_ranges_compose_with_docstring_decoding() {
    for (header, first_prefix, continuation) in [
        ("", "", ""),
        ("", "- ", "  "),
        ("!!! note\n", "    ", "    "),
        ("", "> ", "> "),
    ] {
        let markdown = format!(
            "{header}{first_prefix}[API α\n{continuation}Reference \\u03b2](@ref\n{continuation}Main.\\u0066) and `code\n{continuation}\\u0066`."
        );
        let source = format!(
            "\"\"\"\n    {}\n    \"\"\"\nf() = 1\n",
            markdown.replace('\n', "\n    ")
        );
        for source in [source.clone(), source.replace('\n', "\r\n")] {
            let julia = parse_julia(&source);
            let attachment = julia
                .cst
                .descendants()
                .find_map(DocAttachment::cast)
                .unwrap();
            let DocText::Static(decoded) = attachment.text() else {
                panic!("static docstring");
            };
            let markdown = parse(decoded.as_str());
            assert_eq!(reconstruct(&markdown.cst), decoded.as_str());
            let link = markdown.cst.descendants().find_map(Link::cast).unwrap();
            assert_eq!(link.destination(), "@ref Main.f");
            let reference = link.documenter_link().unwrap();
            assert_eq!(reference.kind(), DocumenterLinkKind::Ref);
            assert_eq!(reference.target(), Some("Main.f"));
            let range = reference.target_range().unwrap();
            assert_eq!(
                &decoded.as_str()[usize::from(range.start())..usize::from(range.end())],
                "Main.f"
            );
            let mapped = decoded.source_map().source_range(range).unwrap();
            assert_eq!(
                &source[usize::from(mapped.start())..usize::from(mapped.end())],
                r"Main.\u0066"
            );
            let mapped = decoded
                .source_map()
                .source_range(link.syntax().text_range())
                .unwrap();
            assert_eq!(usize::from(mapped.start()), source.find("[API").unwrap());
            assert_eq!(usize::from(mapped.end()), source.find(") and").unwrap() + 1);
            let label = link
                .label()
                .find_map(|inline| match inline {
                    Inline::Text(token) if token.text().contains('β') => Some(token),
                    _ => None,
                })
                .unwrap();
            let mapped = decoded
                .source_map()
                .source_range(label.text_range())
                .unwrap();
            assert_eq!(
                &source[usize::from(mapped.start())..usize::from(mapped.end())],
                r"Reference \u03b2"
            );
            let code = markdown
                .cst
                .descendants()
                .find_map(InlineCode::cast)
                .unwrap();
            assert_eq!(code.content(), "code f");
            let content = code
                .syntax()
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.kind() == SyntaxKind::CODE_CONTENT && token.text() == "f")
                .unwrap();
            let mapped = decoded
                .source_map()
                .source_range(content.text_range())
                .unwrap();
            assert_eq!(
                &source[usize::from(mapped.start())..usize::from(mapped.end())],
                r"\u0066"
            );
        }
    }
}

#[test]
fn a_wrapped_documenter_target_covers_its_original_bytes() {
    for newline in ["\n", "\r\n"] {
        let input = format!("- [API](@ref \"API α{newline}  Reference β\")\n");
        let parsed = parse(&input);
        let link = parsed.cst.descendants().find_map(Link::cast).unwrap();
        let destination = link.destination_range().unwrap();
        assert_eq!(
            &input[usize::from(destination.start())..usize::from(destination.end())],
            format!("@ref \"API α{newline}  Reference β\"")
        );
        let reference = link.documenter_link().unwrap();
        assert_eq!(reference.target(), Some("\"API α Reference β\""));
        let target = reference.target_range().unwrap();
        assert_eq!(
            &input[usize::from(target.start())..usize::from(target.end())],
            format!("\"API α{newline}  Reference β\"")
        );
    }
}

#[test]
fn parses_the_julia_markdown_block_dialect() {
    let input = concat!(
        "# Hash heading\n\n",
        "Setext heading\n---\n\n",
        "Paragraph with **bold**, *italic*, `code`, ``math``, $tex$, ",
        "[a link](https://julialang.org), ![alt](image.png), and [^note].\n\n",
        "> quote\n\n",
        "!!! warning \"Careful\"\n",
        "    Nested prose.\n\n",
        "1. one\n2. two\n\n\n",
        "    indented code\n\n",
        "```julia\nx = 1\n```\n\n",
        "```math\nx^2\n```\n\n",
        "[^note]: note text\n\n",
        "| a | b |\n| :-- | --: |\n| 1 | 2 |\n\n",
        "***\n",
    );
    let parsed = parse(input);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(reconstruct(&parsed.cst), input);

    let kinds: Vec<_> = parsed.cst.descendants().map(|node| node.kind()).collect();
    for expected in [
        SyntaxKind::ATX_HEADING,
        SyntaxKind::SETEXT_HEADING,
        SyntaxKind::PARAGRAPH,
        SyntaxKind::STRONG,
        SyntaxKind::EMPHASIS,
        SyntaxKind::INLINE_CODE,
        SyntaxKind::INLINE_MATH,
        SyntaxKind::LINK,
        SyntaxKind::IMAGE,
        SyntaxKind::FOOTNOTE_REFERENCE,
        SyntaxKind::BLOCK_QUOTE,
        SyntaxKind::ADMONITION,
        SyntaxKind::LIST,
        SyntaxKind::LIST_ITEM,
        SyntaxKind::INDENTED_CODE_BLOCK,
        SyntaxKind::FENCED_CODE_BLOCK,
        SyntaxKind::MATH_BLOCK,
        SyntaxKind::FOOTNOTE_DEFINITION,
        SyntaxKind::TABLE,
        SyntaxKind::TABLE_ROW,
        SyntaxKind::TABLE_CELL,
        SyntaxKind::THEMATIC_BREAK,
    ] {
        assert!(kinds.contains(&expected), "missing {expected:?}");
    }
}

#[test]
fn typed_ast_exposes_headings_and_fences() {
    let parsed = parse("## API\n\n```jldoctest shared; output = false\njulia> f(1)\n2\n```\n");
    let document = Document::cast(parsed.cst.clone()).expect("document root");
    let blocks: Vec<_> = document.blocks().collect();

    let Block::Heading(heading) = &blocks[0] else {
        panic!("expected heading, got {:?}", blocks[0].syntax().kind());
    };
    assert_eq!(heading.level(), 2);
    assert_eq!(heading.content(), "API");

    let Block::CodeBlock(fence) = &blocks[1] else {
        panic!("expected code fence, got {:?}", blocks[1].syntax().kind());
    };
    assert_eq!(fence.info(), "jldoctest shared; output = false");
    assert_eq!(fence.content(), "julia> f(1)\n2");
    assert_eq!(
        fence.fence_kind(),
        FenceKind::JlDoctest {
            label: Some("shared".to_string()),
            options: Some("output = false".to_string()),
        }
    );
}

#[test]
fn classifies_all_core_documenter_fences() {
    let cases = [
        ("@docs", DocumenterDirective::Docs),
        ("@autodocs; canonical=false", DocumenterDirective::Autodocs),
        ("@meta", DocumenterDirective::Meta),
        ("@index", DocumenterDirective::Index),
        ("@contents", DocumenterDirective::Contents),
        ("@example demo", DocumenterDirective::Example),
        ("@repl demo", DocumenterDirective::Repl),
        ("@setup demo", DocumenterDirective::Setup),
        ("@eval demo", DocumenterDirective::Eval),
        ("@raw html", DocumenterDirective::Raw),
    ];

    for (info, expected) in cases {
        let source = format!("```{info}\nx = 1\n```\n");
        let parsed = parse(&source);
        let fence = parsed
            .cst
            .descendants()
            .find_map(CodeBlock::cast)
            .expect("code fence");
        let FenceKind::Documenter { directive, .. } = fence.fence_kind() else {
            panic!("{info:?} was not classified as Documenter syntax");
        };
        assert_eq!(directive, expected, "{info}");
    }

    let parsed = parse("```@plotly extension\nx\n```\n");
    let fence = parsed.cst.descendants().find_map(CodeBlock::cast).unwrap();
    assert_eq!(
        fence.fence_kind(),
        FenceKind::DocumenterExtension {
            directive: "plotly".to_string(),
            arguments: Some("extension".to_string()),
        }
    );
}

#[test]
fn classifies_documenter_links_without_resolving_them() {
    let parsed = parse(concat!(
        "[`f`](@ref) [function](@ref Main.f) ",
        "[Header](@id custom-header) [external](@extref Other.label)\n",
    ));
    let links: Vec<_> = parsed.cst.descendants().filter_map(Link::cast).collect();
    assert_eq!(links.len(), 4);

    let inferred = links[0].documenter_link().unwrap();
    assert_eq!(inferred.kind(), DocumenterLinkKind::Ref);
    assert_eq!(inferred.target(), None);
    assert_eq!(links[1].documenter_link().unwrap().target(), Some("Main.f"));
    let target = links[1].documenter_link().unwrap().target_range().unwrap();
    let reconstructed = reconstruct(&parsed.cst);
    assert_eq!(
        &reconstructed[u32::from(target.start()) as usize..u32::from(target.end()) as usize],
        "Main.f"
    );
    assert_eq!(
        links[2].documenter_link().unwrap().kind(),
        DocumenterLinkKind::Id
    );
    assert_eq!(
        links[3].documenter_link().unwrap().kind(),
        DocumenterLinkKind::Extension("extref")
    );
}

#[test]
fn interpolation_is_recognized_but_never_evaluated() {
    let parsed = parse("Value: $name.\n\nValue: $(f(x + 1)).\n");
    let payloads: Vec<_> = parsed
        .cst
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::INTERPOLATION)
        .map(|node| node.text().to_string())
        .collect();
    assert_eq!(payloads, ["$name", "$(f(x + 1))"]);
}

#[test]
fn an_unclosed_explicit_fence_falls_back_losslessly() {
    let input = "```julia\nx = 1\n";
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].kind, DiagnosticKind::UnclosedFence);
    assert!(
        parsed
            .cst
            .descendants()
            .all(|node| node.kind() != SyntaxKind::FENCED_CODE_BLOCK)
    );
}

#[test]
fn a_fence_can_start_immediately_after_a_list_marker() {
    let input = "- ```julia\n  α = 1\n  ```\n- next\n";
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let list = parsed.cst.children().find_map(List::cast).unwrap();
    let first = list.items().next().unwrap();
    assert!(first.inlines().next().is_none());
    let code = first
        .blocks()
        .find_map(|block| match block {
            Block::CodeBlock(code) => Some(code),
            _ => None,
        })
        .unwrap();
    assert_eq!(code.content(), "α = 1");
}

#[test]
fn list_fences_preserve_indented_blank_lines() {
    let input = "- Example:\n  ```julia\n  α = 1\n  \n  \n  β = 2\n  ```\n";
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let code = parsed.cst.descendants().find_map(CodeBlock::cast).unwrap();
    assert_eq!(code.content(), "α = 1\n\n\nβ = 2");
}

#[test]
fn list_fences_preserve_code_indentation_and_decoded_ranges() {
    for (marker, prefix) in [("-", "  "), ("1.", "   "), ("  -", "    ")] {
        for fence in ["```", "~~~~"] {
            for newline in ["\n", "\r\n"] {
                let input = format!(
                    "{marker} Example{newline}{newline}{prefix}{fence}julia{newline}\
                     {prefix}α = 1{newline}{prefix}  β = 2{newline}{prefix}{fence}{newline}\
                     {marker} Next{newline}"
                );
                let parsed = parse(&input);
                assert_eq!(reconstruct(&parsed.cst), input);
                assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
                let list = parsed.cst.children().find_map(List::cast).unwrap();
                let items: Vec<_> = list.items().collect();
                assert_eq!(items.len(), 2);
                let code = items[0]
                    .blocks()
                    .find_map(|block| match block {
                        Block::CodeBlock(code) => Some(code),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("no nested fence in {input:?}"));
                assert_eq!(code.fence_kind(), FenceKind::Julia);
                assert_eq!(code.content(), format!("α = 1{newline}  β = 2"));
                let body = code.content_range().unwrap();
                assert_eq!(
                    &input[usize::from(body.start())..usize::from(body.end())],
                    format!("α = 1{newline}{prefix}  β = 2{newline}")
                );
                let tokens: Vec<_> = code
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .filter(|token| token.kind() == SyntaxKind::CODE_CONTENT)
                    .collect();
                assert_eq!(tokens.len(), 2);
                for token in tokens {
                    let range = token.text_range();
                    assert_eq!(
                        &input[usize::from(range.start())..usize::from(range.end())],
                        token.text()
                    );
                }
            }
        }
    }
}

#[test]
fn list_fences_do_not_consume_siblings_or_indented_code() {
    let input = concat!(
        "- Literal fences\n\n",
        "      ```julia\n      x = 1\n      ```\n",
        "- Missing close\n\n",
        "  ```julia\n  α = 1\n",
        "- Sibling\n\n",
        "  ```julia\n  β = 2\n  ```\n",
    );
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = parsed.diagnostics[0];
    assert_eq!(diagnostic.kind, DiagnosticKind::UnclosedFence);
    assert_eq!(
        usize::from(diagnostic.range.start()),
        input.find("  ```julia\n  α").unwrap() + 2
    );
    let list = parsed.cst.children().find_map(List::cast).unwrap();
    let items: Vec<_> = list.items().collect();
    assert_eq!(items.len(), 3);
    assert!(
        items[0]
            .blocks()
            .any(|block| matches!(block, Block::IndentedCodeBlock(_)))
    );
    assert!(
        !items[1]
            .blocks()
            .any(|block| matches!(block, Block::CodeBlock(_)))
    );
    assert!(
        items[2]
            .blocks()
            .any(|block| matches!(block, Block::CodeBlock(_)))
    );
}

#[test]
fn list_fence_code_ranges_compose_with_escaped_docstring_bytes() {
    let source = r#"
"""
- Example:
  ```julia
  α = 1
  \u0066(α)
  ```
"""
f() = 1
"#;
    let parsed = parse_julia(source);
    let attachment = parsed
        .cst
        .descendants()
        .find_map(DocAttachment::cast)
        .unwrap();
    let DocText::Static(decoded) = attachment.text() else {
        panic!("expected a static docstring");
    };
    let markdown = parse(decoded.as_str());
    let code = markdown
        .cst
        .descendants()
        .find_map(CodeBlock::cast)
        .unwrap();
    assert_eq!(code.content(), "α = 1\nf(α)");
    let second_line = code
        .syntax()
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::CODE_CONTENT)
        .nth(1)
        .unwrap();
    let name = rowan::TextRange::at(second_line.text_range().start(), rowan::TextSize::new(1));
    let original = decoded.source_map().source_range(name).unwrap();
    assert_eq!(
        &source[usize::from(original.start())..usize::from(original.end())],
        r"\u0066"
    );
}

#[test]
fn admonition_fences_preserve_classification_indentation_and_ranges() {
    for prefix in ["    ", "\t"] {
        for newline in ["\n", "\r\n"] {
            for (fence, info, kind) in [
                (
                    "```",
                    "@example gallery",
                    FenceKind::Documenter {
                        directive: DocumenterDirective::Example,
                        arguments: Some("gallery".to_string()),
                    },
                ),
                ("~~~~", "julia", FenceKind::Julia),
                ("```", "julia-repl", FenceKind::JuliaRepl),
                (
                    "~~~",
                    "jldoctest gallery",
                    FenceKind::JlDoctest {
                        label: Some("gallery".to_string()),
                        options: None,
                    },
                ),
                ("```", "python", FenceKind::Other("python".to_string())),
                ("~~~", "", FenceKind::Plain),
            ] {
                let input = format!(
                    "!!! note \"Gallery\"{newline}\
                     {prefix}Before.{newline}\
                     {prefix}{fence}{info}{newline}\
                     {prefix}α = 1{newline}\
                     {prefix}    β = 2{newline}\
                     {prefix}{fence}{newline}\
                     {prefix}After.{newline}\
                     Outside.{newline}"
                );
                let parsed = parse(&input);
                assert_eq!(reconstruct(&parsed.cst), input);
                assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
                let document = Document::cast(parsed.cst).unwrap();
                let blocks: Vec<_> = document.blocks().collect();
                assert_eq!(blocks.len(), 2);
                let Block::Admonition(admonition) = &blocks[0] else {
                    panic!("expected an admonition");
                };
                let blocks: Vec<_> = admonition.blocks().collect();
                assert_eq!(blocks.len(), 3, "{input:?}");
                assert!(matches!(blocks[0], Block::Paragraph(_)));
                assert!(matches!(blocks[2], Block::Paragraph(_)));
                let Block::CodeBlock(code) = &blocks[1] else {
                    panic!("expected a nested fence in {input:?}");
                };
                assert_eq!(code.info(), info);
                assert_eq!(code.fence_kind(), kind);
                let delimiters: Vec<_> = code
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .filter(|token| token.kind() == SyntaxKind::FENCE)
                    .map(|token| token.text().to_string())
                    .collect();
                assert_eq!(delimiters, [fence, fence]);
                assert_eq!(code.content(), format!("α = 1{newline}    β = 2"));
                let body = code.content_range().unwrap();
                assert_eq!(usize::from(body.start()), input.find("α = 1").unwrap());
                assert_eq!(
                    &input[usize::from(body.start())..usize::from(body.end())],
                    format!("α = 1{newline}{prefix}    β = 2{newline}")
                );
                let tokens: Vec<_> = code
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .filter(|token| token.kind() == SyntaxKind::CODE_CONTENT)
                    .collect();
                assert_eq!(tokens.len(), 2);
                assert_eq!(tokens[0].text(), format!("α = 1{newline}"));
                assert_eq!(tokens[1].text(), format!("    β = 2{newline}"));
                for token in document
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    let range = token.text_range();
                    assert_eq!(
                        &input[usize::from(range.start())..usize::from(range.end())],
                        token.text()
                    );
                }
            }
        }
    }
}

#[test]
fn admonition_fences_keep_blank_lines_and_mixed_container_prefixes() {
    let input = concat!(
        "!!! note\n",
        "    ```@example gallery\n",
        "\tα = 1\n",
        "\n\n",
        "      β = 2\n",
        "\t```\n",
    );
    for input in [input.to_string(), input.replace('\n', "\r\n")] {
        let parsed = parse(&input);
        assert_eq!(reconstruct(&parsed.cst), input);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let code = parsed.cst.descendants().find_map(CodeBlock::cast).unwrap();
        let newline = if input.contains('\r') { "\r\n" } else { "\n" };
        assert_eq!(
            code.content(),
            format!("α = 1{newline}{newline}{newline}  β = 2")
        );
    }
}

#[test]
fn admonition_fences_do_not_consume_outside_blocks() {
    let input = concat!(
        "!!! note \"Missing close\"\n",
        "    ```julia\n    α = 1\n",
        "```julia\nβ = 2\n```\n",
        "!!! tip\n    ```@example gallery\n    γ = 3\n    ```\n",
    );
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = parsed.diagnostics[0];
    assert_eq!(diagnostic.kind, DiagnosticKind::UnclosedFence);
    assert_eq!(
        usize::from(diagnostic.range.start()),
        input.find("```julia").unwrap()
    );
    assert_eq!(diagnostic.range.len(), rowan::TextSize::new(3));
    let document = Document::cast(parsed.cst).unwrap();
    let blocks: Vec<_> = document.blocks().collect();
    assert_eq!(blocks.len(), 3);
    let Block::Admonition(first) = &blocks[0] else {
        panic!("expected an admonition");
    };
    assert!(
        !first
            .blocks()
            .any(|block| matches!(block, Block::CodeBlock(_)))
    );
    let Block::CodeBlock(outside) = &blocks[1] else {
        panic!("expected an outside fence");
    };
    assert_eq!(outside.content(), "β = 2");
    let Block::Admonition(last) = &blocks[2] else {
        panic!("expected a sibling admonition");
    };
    let code = last
        .blocks()
        .find_map(|block| match block {
            Block::CodeBlock(code) => Some(code),
            _ => None,
        })
        .unwrap();
    assert_eq!(code.content(), "γ = 3");
}

#[test]
fn admonition_fence_ranges_compose_with_escaped_dedented_docstrings() {
    let source = r#""""
    !!! note "Gallery"
        ```@example gallery
        α = 1
            \u0066(\u03b1)
        ```
    """
f() = 1
"#;
    for source in [source.to_string(), source.replace('\n', "\r\n")] {
        let julia = parse_julia(&source);
        assert!(julia.diagnostics.is_empty());
        let attachment = julia
            .cst
            .descendants()
            .find_map(DocAttachment::cast)
            .unwrap();
        let DocText::Static(decoded) = attachment.text() else {
            panic!("expected a static docstring");
        };
        let markdown = parse(decoded.as_str());
        assert_eq!(reconstruct(&markdown.cst), decoded.as_str());
        let admonition = markdown.cst.children().find_map(Admonition::cast).unwrap();
        let code = admonition
            .blocks()
            .find_map(|block| match block {
                Block::CodeBlock(code) => Some(code),
                _ => None,
            })
            .unwrap();
        assert_eq!(code.content(), "α = 1\n    f(α)");
        let body = code.content_range().unwrap();
        let original = decoded.source_map().source_range(body).unwrap();
        let newline = if source.contains('\r') { "\r\n" } else { "\n" };
        assert_eq!(
            &source[usize::from(original.start())..usize::from(original.end())],
            format!("α = 1{newline}            \\u0066(\\u03b1){newline}")
        );
        let second_line = code
            .syntax()
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::CODE_CONTENT)
            .nth(1)
            .unwrap();
        for (offset, length, spelling) in [(4, 1, r"\u0066"), (6, 2, r"\u03b1")] {
            let range = rowan::TextRange::at(
                second_line.text_range().start() + rowan::TextSize::new(offset),
                rowan::TextSize::new(length),
            );
            let original = decoded.source_map().source_range(range).unwrap();
            assert_eq!(
                &source[usize::from(original.start())..usize::from(original.end())],
                spelling
            );
        }
    }
}

#[test]
fn typed_ast_casts_are_total_over_document_blocks() {
    let parsed = parse("# H\n\ntext\n\n---\n");
    let document = Document::cast(parsed.cst).unwrap();
    let blocks: Vec<_> = document.blocks().collect();
    assert!(matches!(blocks[0], Block::Heading(_)));
    assert!(matches!(blocks[1], Block::Paragraph(_)));
    assert!(matches!(blocks[2], Block::ThematicBreak(_)));

    assert!(Heading::cast(blocks[1].syntax().clone()).is_none());
}

#[test]
fn typed_inline_and_container_accessors_cover_the_public_tree() {
    let parsed = parse("- text with **weight**, `code`, and [^note]\n");
    let list = parsed.cst.children().find_map(List::cast).expect("list");
    assert_eq!(list.ordered_start(), None);
    let item = list.items().next().expect("list item");
    let inlines: Vec<_> = item.inlines().collect();
    assert!(matches!(inlines[0], Inline::Text(_)));
    assert!(
        inlines
            .iter()
            .any(|inline| matches!(inline, Inline::Strong(_)))
    );
    let code = inlines
        .iter()
        .find_map(|inline| match inline {
            Inline::Code(code) => Some(code),
            _ => None,
        })
        .expect("inline code");
    assert_eq!(code.content(), "code");
    let footnote = inlines
        .iter()
        .find_map(|inline| match inline {
            Inline::FootnoteReference(footnote) => Some(footnote),
            _ => None,
        })
        .expect("footnote reference");
    assert_eq!(footnote.id(), "note");
}

#[test]
fn decoded_ranges_compose_with_the_docstring_source_map() {
    let source = r#"
"""
    See [f](@ref Main.\u0066).
    """
f() = 1
"#;
    let julia = parse_julia(source);
    let attachment = julia
        .cst
        .descendants()
        .find_map(DocAttachment::cast)
        .expect("docstring attachment");
    let DocText::Static(decoded) = attachment.text() else {
        panic!("expected statically decoded documentation");
    };
    let markdown = parse(decoded.as_str());
    let link = markdown
        .cst
        .descendants()
        .find_map(Link::cast)
        .unwrap_or_else(|| panic!("no link in decoded documentation: {:?}", decoded.as_str()));
    let documenter = link.documenter_link().expect("Documenter link");
    let mapped = decoded
        .source_map()
        .source_range(documenter.target_range().expect("explicit target"))
        .expect("mapped Documenter target");
    let start = u32::from(mapped.start()) as usize;
    let end = u32::from(mapped.end()) as usize;
    assert_eq!(&source[start..end], r#"Main.\u0066"#);
}
#[test]
fn heading_slugs_come_from_one_definition() {
    let parsed = parse("# The Nelder--Mead *Method*---revisited\n");
    let heading = parsed
        .cst
        .descendants()
        .find_map(Heading::cast)
        .expect("heading");
    assert_eq!(heading.content(), "The Nelder–Mead Method—revisited");
    assert_eq!(heading.slug(), "the-neldermead-methodrevisited");
    assert_eq!(
        fatou_parser::documentation::ast::slug(&heading.content()),
        heading.slug()
    );
}

#[test]
fn inline_dashes_preserve_spelling_and_prefer_three_hyphens() {
    let input = "a--b a---b a----b a-----b a------b\n";
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    assert!(parsed.diagnostics.is_empty());
    let document = Document::cast(parsed.cst).expect("document");
    let Block::Paragraph(paragraph) = document.blocks().next().expect("paragraph") else {
        panic!("expected paragraph");
    };
    let dashes: Vec<_> = paragraph
        .inlines()
        .filter_map(|inline| match inline {
            Inline::EnDash(token) => Some(("en", token.text().to_string())),
            Inline::EmDash(token) => Some(("em", token.text().to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(
        dashes,
        [
            ("en", "--".to_string()),
            ("em", "---".to_string()),
            ("em", "---".to_string()),
            ("em", "---".to_string()),
            ("en", "--".to_string()),
            ("em", "---".to_string()),
            ("em", "---".to_string()),
        ]
    );
}
#[test]
fn a_nested_image_does_not_capture_its_enclosing_links_destination() {
    let parsed = parse("[![build](https://img.example/badge.svg)](https://ci.example/job)\n");
    let links: Vec<Link> = parsed.cst.descendants().filter_map(Link::cast).collect();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].destination(), "https://ci.example/job");

    let referenced = parse("[![icon](icon.svg)](@ref Base.sum)\n");
    let link = referenced
        .cst
        .descendants()
        .find_map(Link::cast)
        .expect("link");
    let documenter = link.documenter_link().expect("Documenter link");
    assert_eq!(documenter.kind(), DocumenterLinkKind::Ref);
    assert_eq!(documenter.target(), Some("Base.sum"));
}

#[test]
fn an_underline_only_makes_a_heading_out_of_an_unclaimed_line() {
    // Julia's flavor tries setext last, so a list or a quote wins the line and
    // the underline degrades to a thematic break.
    for (input, first) in [
        ("- item\n---\n", SyntaxKind::LIST),
        ("> quoted\n---\n", SyntaxKind::BLOCK_QUOTE),
        ("!!! note\n    body\n---\n", SyntaxKind::ADMONITION),
        ("[^a]: note\n---\n", SyntaxKind::FOOTNOTE_DEFINITION),
    ] {
        let blocks: Vec<_> = parse(input)
            .cst
            .children()
            .map(|node| node.kind())
            .collect();
        assert_eq!(
            blocks,
            vec![first, SyntaxKind::THEMATIC_BREAK],
            "input {input:?}"
        );
    }

    let plain: Vec<_> = parse("para\n---\n")
        .cst
        .children()
        .map(|node| node.kind())
        .collect();
    assert_eq!(plain, vec![SyntaxKind::SETEXT_HEADING]);
}

#[test]
fn an_admonition_title_drops_its_quotes_and_trailing_space() {
    let input = "!!! note \"Careful\"  \n    body\n";
    let parsed = parse(input);
    assert_eq!(reconstruct(&parsed.cst), input);
    let admonition = parsed
        .cst
        .descendants()
        .find_map(fatou_parser::documentation::ast::Admonition::cast)
        .expect("admonition");
    assert_eq!(admonition.title(), "Careful");
}
