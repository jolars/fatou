use fatou_parser::documentation::ast::{
    Block, CodeBlock, Document, DocumenterDirective, DocumenterLinkKind, FenceKind, Heading,
    Inline, Link, List,
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
    let input = "α [`β`](@ref Γ.δ) -- $x$\n!!! note \"λ\"\n    `μ` and $(f(ν))\n";
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
    let parsed = parse("# The Nelder--Mead *Method*\n");
    let heading = parsed
        .cst
        .descendants()
        .find_map(Heading::cast)
        .expect("heading");
    assert_eq!(heading.content(), "The Nelder–Mead Method");
    assert_eq!(heading.slug(), "the-neldermead-method");
    assert_eq!(
        fatou_parser::documentation::ast::slug(&heading.content()),
        heading.slug()
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
