//! Differential oracle against pinned Julia `Markdown.parse` projections.
//!
//! Julia is used only by `scripts/update-markdown-corpus.jl`; this test reads
//! committed artifacts and is therefore safe in ordinary builds and CI.

use std::fs;
use std::path::{Path, PathBuf};

use fatou_parser::documentation::ast::CodeBlock;
use fatou_parser::documentation::parse;
use fatou_parser::documentation::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use rowan::ast::AstNode as _;

const CORPUS_REL: &str = "tests/fixtures/documentation/oracle";

#[test]
fn markdown_oracle_corpus_is_present_and_pinned() {
    let root = corpus_root();
    assert!(root.join(".markdown-source").is_file());
    assert!(read_cases().len() >= 15);
}

#[test]
fn matches_julia_markdown_semantic_shape() {
    let mut failures = Vec::new();
    for case in read_cases() {
        let parsed = parse(&case.input);
        let actual = render_document(&parsed.cst);
        if actual != case.expected.trim() {
            failures.push(format!(
                "{}\n  expected: {}\n    actual: {}",
                case.slug,
                case.expected.trim(),
                actual
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Julia Markdown oracle divergence(s):\n{}",
        failures.join("\n")
    );
}

struct Case {
    slug: String,
    input: String,
    expected: String,
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS_REL)
}

fn read_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for entry in fs::read_dir(corpus_root()).expect("read Markdown oracle corpus") {
        let entry = entry.expect("read corpus entry");
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let input = entry.path().join("input.md");
        let expected = entry.path().join("expected.sexpr");
        if input.is_file() && expected.is_file() {
            cases.push(Case {
                slug: entry.file_name().to_string_lossy().to_string(),
                input: fs::read_to_string(input).expect("read Markdown fixture"),
                expected: fs::read_to_string(expected).expect("read Markdown projection"),
            });
        }
    }
    cases.sort_by(|left, right| left.slug.cmp(&right.slug));
    cases
}

fn render_document(root: &SyntaxNode) -> String {
    format!(
        "(document {})",
        root.children()
            .map(|node| render_block(&node))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn render_block(node: &SyntaxNode) -> String {
    match node.kind() {
        SyntaxKind::PARAGRAPH => format!("(paragraph {})", render_inlines(node)),
        SyntaxKind::ATX_HEADING => {
            let level = first_token(node, SyntaxKind::HEADING_MARKER)
                .bytes()
                .filter(|&byte| byte == b'#')
                .count();
            format!("(heading {level} {})", render_inlines(node))
        }
        SyntaxKind::SETEXT_HEADING => {
            let level = if first_token(node, SyntaxKind::HEADING_MARKER)
                .trim_start()
                .starts_with('=')
            {
                1
            } else {
                2
            };
            format!("(heading {level} {})", render_inlines(node))
        }
        SyntaxKind::FENCED_CODE_BLOCK => {
            let block = CodeBlock::cast(node.clone()).expect("fenced code block");
            format!("(code {} {})", hex(&block.info()), hex(&block.content()))
        }
        SyntaxKind::INDENTED_CODE_BLOCK => {
            format!("(code  {})", hex(&indented_code(node)))
        }
        SyntaxKind::MATH_BLOCK => {
            let formula = first_token(node, SyntaxKind::MATH_CONTENT);
            format!("(math {})", hex(formula.trim_end_matches(['\r', '\n'])))
        }
        SyntaxKind::BLOCK_QUOTE => format!(
            "(blockquote {})",
            node.children()
                .map(|child| render_block(&child))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        SyntaxKind::ADMONITION => {
            let category = first_token(node, SyntaxKind::ADMONITION_CATEGORY);
            let raw_title = first_token(node, SyntaxKind::ADMONITION_TITLE);
            let title = if raw_title.is_empty() {
                let mut chars = category.chars();
                chars
                    .next()
                    .map(char::to_uppercase)
                    .into_iter()
                    .flatten()
                    .collect::<String>()
                    + chars.as_str()
            } else {
                raw_title.trim_matches('"').to_string()
            };
            format!(
                "(admonition {} {} {})",
                hex(&category),
                hex(&title),
                node.children()
                    .map(|child| render_block(&child))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        SyntaxKind::LIST => render_list(node),
        SyntaxKind::FOOTNOTE_DEFINITION => {
            let raw = first_token(node, SyntaxKind::FOOTNOTE_LABEL);
            let id = raw
                .strip_prefix("[^")
                .and_then(|text| text.strip_suffix("]:"));
            format!(
                "(footnote-def {} {})",
                hex(id.unwrap_or(&raw)),
                node.children()
                    .map(|child| render_block(&child))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        SyntaxKind::TABLE => render_table(node),
        SyntaxKind::THEMATIC_BREAK => "(thematic-break)".to_string(),
        SyntaxKind::INTERPOLATION => "(interpolation)".to_string(),
        other => panic!("unprojected documentation block {other:?}"),
    }
}

fn render_list(node: &SyntaxNode) -> String {
    let marker = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::LIST_MARKER)
        .expect("list marker");
    let ordered = marker
        .text()
        .trim_end_matches(['.', ')'])
        .parse::<i64>()
        .unwrap_or(-1);
    let items = node
        .children()
        .filter(|child| child.kind() == SyntaxKind::LIST_ITEM)
        .map(|item| {
            let mut contents = vec![format!("(paragraph {})", render_inlines(&item))];
            contents.extend(
                item.children()
                    .filter(|child| {
                        matches!(
                            child.kind(),
                            SyntaxKind::LIST | SyntaxKind::INDENTED_CODE_BLOCK
                        )
                    })
                    .map(|child| render_block(&child)),
            );
            format!("(item {})", contents.join(" "))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let loose = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.kind() == SyntaxKind::BLANK_LINE);
    format!("(list {ordered} {loose} {items})")
}

fn render_table(node: &SyntaxNode) -> String {
    let rows: Vec<_> = node
        .children()
        .filter(|child| child.kind() == SyntaxKind::TABLE_ROW)
        .collect();
    let alignment = rows
        .get(1)
        .map(|row| {
            row.children()
                .filter(|cell| cell.kind() == SyntaxKind::TABLE_CELL)
                .map(|cell| {
                    let text = cell.text().to_string();
                    let text = text.trim();
                    match (text.starts_with(':'), text.ends_with(':')) {
                        (true, true) => "c",
                        (true, false) => "l",
                        (false, _) => "r",
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let rendered = rows
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, row)| {
            let cells = row
                .children()
                .filter(|cell| cell.kind() == SyntaxKind::TABLE_CELL)
                .map(|cell| {
                    let content = cell.text().to_string();
                    format!("(cell (text {}))", hex(content.trim()))
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("(row {cells})")
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("(table {alignment} {rendered})")
}

fn render_inlines(node: &SyntaxNode) -> String {
    enum Part {
        Text(String),
        Node(String),
    }

    let mut parts: Vec<Part> = Vec::new();
    for element in node.children_with_tokens() {
        if let rowan::NodeOrToken::Token(token) = &element
            && token.kind() == SyntaxKind::TEXT
        {
            match parts.last_mut() {
                Some(Part::Text(text)) => text.push_str(token.text()),
                _ => parts.push(Part::Text(token.text().to_string())),
            }
            continue;
        }
        if let rowan::NodeOrToken::Token(token) = &element
            && token.kind() == SyntaxKind::SOFT_BREAK
        {
            if token.text_range().end() < node.text_range().end() {
                match parts.last_mut() {
                    Some(Part::Text(text)) => text.push(' '),
                    _ => parts.push(Part::Text(" ".to_string())),
                }
            }
            continue;
        }
        if let Some(rendered) = render_inline_element(element) {
            parts.push(Part::Node(rendered));
        }
    }
    parts
        .into_iter()
        .map(|part| match part {
            Part::Text(text) => format!("(text {})", hex(&text)),
            Part::Node(rendered) => rendered,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_inline_element(element: SyntaxElement) -> Option<String> {
    match element {
        rowan::NodeOrToken::Token(token) => match token.kind() {
            SyntaxKind::TEXT => unreachable!("plain text is coalesced by `render_inlines`"),
            SyntaxKind::ESCAPE => Some(format!(
                "(text {})",
                hex(token.text().trim_start_matches('\\'))
            )),
            SyntaxKind::EN_DASH => Some(format!("(text {})", hex("–"))),
            SyntaxKind::SOFT_BREAK => None,
            _ => None,
        },
        rowan::NodeOrToken::Node(node) => match node.kind() {
            SyntaxKind::EMPHASIS => Some(format!("(emphasis {})", render_inlines(&node))),
            SyntaxKind::STRONG => Some(format!("(strong {})", render_inlines(&node))),
            SyntaxKind::INLINE_CODE => Some(format!(
                "(code  {})",
                hex(first_token(&node, SyntaxKind::CODE_CONTENT).trim())
            )),
            SyntaxKind::INLINE_MATH => Some(format!(
                "(math {})",
                hex(first_token(&node, SyntaxKind::MATH_CONTENT).trim())
            )),
            SyntaxKind::LINK => {
                let destination = first_token(&node, SyntaxKind::LINK_DESTINATION);
                Some(format!(
                    "(link ({}) {})",
                    render_inlines(&node),
                    hex(&destination)
                ))
            }
            SyntaxKind::IMAGE => {
                let alt = first_token(&node, SyntaxKind::TEXT);
                let destination = first_token(&node, SyntaxKind::LINK_DESTINATION);
                Some(format!("(image {} {})", hex(&alt), hex(&destination)))
            }
            SyntaxKind::AUTOLINK => {
                let destination = first_token(&node, SyntaxKind::LINK_DESTINATION);
                Some(format!(
                    "(link ((text {})) {})",
                    hex(&destination),
                    hex(&destination)
                ))
            }
            SyntaxKind::FOOTNOTE_REFERENCE => Some(format!(
                "(footnote-ref {})",
                hex(&first_token(&node, SyntaxKind::FOOTNOTE_LABEL))
            )),
            SyntaxKind::INTERPOLATION => Some("(interpolation)".to_string()),
            SyntaxKind::HARD_BREAK => Some("(linebreak)".to_string()),
            _ => None,
        },
    }
}

fn indented_code(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for element in node.children_with_tokens() {
        let rowan::NodeOrToken::Token(token) = element else {
            continue;
        };
        match token.kind() {
            SyntaxKind::CODE_CONTENT | SyntaxKind::NEWLINE => out.push_str(token.text()),
            _ => {}
        }
    }
    out.trim_end_matches(['\r', '\n']).to_string()
}

fn first_token(node: &SyntaxNode, kind: SyntaxKind) -> String {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == kind)
        .map(|token| token.text().to_string())
        .unwrap_or_default()
}

fn hex(text: &str) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}
