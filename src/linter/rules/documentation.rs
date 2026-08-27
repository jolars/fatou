//! Shared extraction for documentation-aware lint rules.
//!
//! Static docstrings contain their own Markdown CST, so the three documentation
//! rules share this once-per-file scan rather than parsing every attachment
//! independently. The scan records only shapes whose meaning is unambiguous:
//! explicit Julia-bearing fences, conventional ``- `name`: ...`` argument
//! entries, and explicit code-labeled `@ref` destinations.

use std::collections::BTreeSet;

use fatou_parser::documentation::ast::{
    Block, CodeBlock, Document, DocumenterDirective, DocumenterLinkKind, FenceKind, Heading,
    Inline, Link,
};
use rowan::ast::AstNode as _;
use rowan::{TextRange, TextSize};

use crate::ast::{AstToken as _, DocText, Expr, Root};
use crate::parser;
use crate::semantic::{BindingKind, ScopeKind, SemanticDoc, SemanticModel};

#[derive(Debug)]
pub(crate) struct InvalidDocstringCode {
    pub(crate) range: TextRange,
    pub(crate) message: String,
}

#[derive(Debug)]
pub(crate) struct DocstringArgumentMismatch {
    pub(crate) range: TextRange,
    pub(crate) name: String,
}

#[derive(Debug)]
pub(crate) struct DocstringReference {
    pub(crate) range: TextRange,
    pub(crate) target: String,
    pub(crate) at: TextSize,
}

#[derive(Debug, Default)]
pub(crate) struct DocumentationScan {
    pub(crate) invalid_code: Vec<InvalidDocstringCode>,
    pub(crate) argument_mismatches: Vec<DocstringArgumentMismatch>,
    pub(crate) references: Vec<DocstringReference>,
    pub(crate) anchors: BTreeSet<String>,
}

impl DocumentationScan {
    pub(crate) fn collect(model: &SemanticModel) -> Self {
        let mut scan = Self::default();
        for attachment in model.documentation() {
            let DocText::Static(decoded) = &attachment.text else {
                continue;
            };
            let markdown = fatou_parser::documentation::parse(decoded.as_str());
            // Anchors are an exemption set, so they are collected even from a
            // malformed docstring: withholding them would turn one unclosed
            // fence into false `@ref` diagnostics elsewhere in the file.
            collect_anchors(&markdown.cst, &mut scan.anchors);
            // The findings themselves need a clean parse: a malformed Markdown
            // tree is recoverable for editing features, but not strong enough
            // evidence for a correctness diagnostic.
            if !markdown.diagnostics.is_empty() {
                continue;
            }

            collect_code_problems(&markdown.cst, decoded, &mut scan.invalid_code);
            collect_argument_mismatches(
                &markdown.cst,
                decoded,
                attachment,
                model,
                &mut scan.argument_mismatches,
            );
            collect_references(
                &markdown.cst,
                decoded,
                attachment.target_range.start(),
                &mut scan.references,
            );
        }
        scan
    }
}

fn collect_code_problems(
    root: &fatou_parser::documentation::syntax::SyntaxNode,
    decoded: &crate::ast::StaticDocText,
    out: &mut Vec<InvalidDocstringCode>,
) {
    for fence in root.descendants().filter_map(CodeBlock::cast) {
        let Some(content_range) = fence.content_range() else {
            continue;
        };
        match fence.fence_kind() {
            FenceKind::Julia => check_mapped_julia(
                MappedText::direct(decoded.as_str(), content_range),
                decoded,
                out,
            ),
            FenceKind::JuliaRepl | FenceKind::JlDoctest { .. } => {
                check_repl_fence(decoded.as_str(), content_range, decoded, out)
            }
            FenceKind::Documenter { directive, .. } if directive != DocumenterDirective::Raw => {
                check_mapped_julia(
                    MappedText::direct(decoded.as_str(), content_range),
                    decoded,
                    out,
                )
            }
            FenceKind::Plain
            | FenceKind::Documenter { .. }
            | FenceKind::DocumenterExtension { .. }
            | FenceKind::Other(_) => {}
        }
    }
}

fn check_repl_fence(
    text: &str,
    content_range: TextRange,
    decoded: &crate::ast::StaticDocText,
    out: &mut Vec<InvalidDocstringCode>,
) {
    let start = usize::from(content_range.start());
    let end = usize::from(content_range.end());
    let Some(_) = text.get(start..end) else {
        return;
    };
    let lines = line_ranges(text, start, end);
    let has_prompt = lines
        .iter()
        .any(|line| line.content(text).starts_with("julia> "));

    if !has_prompt {
        // Documenter's script-style doctest treats everything before the
        // exact separator as Julia and everything after it as expected output.
        let Some(separator) = lines
            .iter()
            .find(|line| line.content(text).trim() == "# output")
        else {
            return;
        };
        let range = TextRange::new(content_range.start(), TextSize::new(separator.start as u32));
        check_mapped_julia(MappedText::direct(text, range), decoded, out);
        return;
    }

    let mut line_index = 0;
    while line_index < lines.len() {
        let line = lines[line_index];
        let body = line.content(text);
        let Some(code) = body.strip_prefix("julia> ") else {
            line_index += 1;
            continue;
        };

        let code_start = line.content_end - code.len();
        let mut snippet = MappedText::default();
        snippet.push_slice(text, code_start, line.end);
        let mut parsed = parser::parse(&snippet.text);
        let mut next = line_index + 1;

        // A clean first line is a complete input, so subsequent lines are
        // output. An incomplete input may consume the indented continuation
        // lines the REPL prints until it becomes clean or the transcript stops.
        while !parsed.diagnostics.is_empty() && next < lines.len() {
            let continuation = lines[next];
            let continuation_text = continuation.content(text);
            if continuation_text.starts_with("julia> ")
                || !(continuation_text.starts_with("       ")
                    || continuation_text.starts_with('\t'))
            {
                break;
            }
            snippet.push_slice(text, continuation.start, continuation.end);
            parsed = parser::parse(&snippet.text);
            next += 1;
        }
        record_parse_diagnostics(&snippet, &parsed.diagnostics, decoded, out);
        line_index = next;
    }
}

fn check_mapped_julia(
    snippet: MappedText,
    decoded: &crate::ast::StaticDocText,
    out: &mut Vec<InvalidDocstringCode>,
) {
    let parsed = parser::parse(&snippet.text);
    record_parse_diagnostics(&snippet, &parsed.diagnostics, decoded, out);
}

fn record_parse_diagnostics(
    snippet: &MappedText,
    diagnostics: &[crate::parser::ParseDiagnostic],
    decoded: &crate::ast::StaticDocText,
    out: &mut Vec<InvalidDocstringCode>,
) {
    for diagnostic in diagnostics {
        let Some(decoded_range) = snippet.source_range(diagnostic.start, diagnostic.end) else {
            continue;
        };
        let Some(range) = decoded.source_map().source_range(decoded_range) else {
            continue;
        };
        out.push(InvalidDocstringCode {
            range,
            message: diagnostic.message.clone(),
        });
    }
}

fn collect_argument_mismatches(
    root: &fatou_parser::documentation::syntax::SyntaxNode,
    decoded: &crate::ast::StaticDocText,
    attachment: &SemanticDoc,
    model: &SemanticModel,
    out: &mut Vec<DocstringArgumentMismatch>,
) {
    let Some(parameters) = documented_parameters(attachment, model) else {
        return;
    };
    let Some(document) = Document::cast(root.clone()) else {
        return;
    };
    let mut arguments_level = None;
    for block in document.blocks() {
        if let Block::Heading(heading) = &block {
            if heading.content().trim() == "Arguments" {
                arguments_level = Some(heading.level());
                continue;
            }
            if arguments_level.is_some_and(|level| heading.level() <= level) {
                arguments_level = None;
            }
            continue;
        }
        if arguments_level.is_none() {
            continue;
        }
        let Block::List(list) = block else {
            continue;
        };
        for item in list.items() {
            let Some(Inline::Code(code)) = item.inlines().next() else {
                continue;
            };
            let Some(name) = simple_argument_name(&code.content()) else {
                continue;
            };
            if parameters.contains(&name) {
                continue;
            }
            let Some(range) = decoded
                .source_map()
                .source_range(code.syntax().text_range())
            else {
                continue;
            };
            out.push(DocstringArgumentMismatch { range, name });
        }
    }
}

fn documented_parameters(
    attachment: &SemanticDoc,
    model: &SemanticModel,
) -> Option<BTreeSet<String>> {
    let binding = model.binding(attachment.binding?);
    if !matches!(binding.kind, BindingKind::Function | BindingKind::Macro) {
        return None;
    }
    let scope = model.scopes().iter().find(|scope| {
        scope.kind == ScopeKind::Function && scope.range == attachment.target_range
    })?;
    Some(
        scope
            .bindings
            .iter()
            .map(|&id| model.binding(id))
            .filter(|binding| {
                matches!(binding.kind, BindingKind::Param | BindingKind::KeywordParam)
            })
            .map(|binding| binding.name.to_string())
            .collect(),
    )
}

fn simple_argument_name(text: &str) -> Option<String> {
    let parsed = parser::parse(text);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let root = Root::cast(parsed.cst)?;
    let mut items = root.items();
    let item = items.next()?;
    if items.next().is_some() {
        return None;
    }
    let name = match item {
        Expr::Name(name) => name,
        Expr::SplatExpr(splat) => match splat.expr()? {
            Expr::Name(name) => name,
            _ => return None,
        },
        _ => return None,
    };
    Some(name.ident()?.text().to_string())
}

fn collect_references(
    root: &fatou_parser::documentation::syntax::SyntaxNode,
    decoded: &crate::ast::StaticDocText,
    at: TextSize,
    out: &mut Vec<DocstringReference>,
) {
    for link in root.descendants().filter_map(Link::cast) {
        let Some(documenter) = link.documenter_link() else {
            continue;
        };
        if documenter.kind() != DocumenterLinkKind::Ref
            || !matches!(link.label().next(), Some(Inline::Code(_)))
        {
            continue;
        }
        let (Some(target), Some(decoded_range)) = (documenter.target(), documenter.target_range())
        else {
            continue;
        };
        let Some(range) = decoded.source_map().source_range(decoded_range) else {
            continue;
        };
        out.push(DocstringReference {
            range,
            target: target.to_string(),
            at,
        });
    }
}

fn collect_anchors(
    root: &fatou_parser::documentation::syntax::SyntaxNode,
    out: &mut BTreeSet<String>,
) {
    for node in root.descendants() {
        if let Some(link) = Link::cast(node.clone())
            && let Some(documenter) = link.documenter_link()
            && documenter.kind() == DocumenterLinkKind::Id
            && let Some(target) = documenter.target()
        {
            out.insert(target.to_string());
        }
        if let Some(heading) = Heading::cast(node) {
            let content = heading.content();
            if !content.is_empty() {
                out.insert(content.clone());
                let slug = heading.slug();
                if !slug.is_empty() {
                    out.insert(slug);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LineRange {
    start: usize,
    content_end: usize,
    end: usize,
}

impl LineRange {
    fn content(self, text: &str) -> &str {
        &text[self.start..self.content_end]
    }
}

fn line_ranges(text: &str, base: usize, end: usize) -> Vec<LineRange> {
    let mut lines = Vec::new();
    let mut start = base;
    for line in text[base..end].split_inclusive('\n') {
        let line_end = start + line.len();
        let content_end =
            line_end - usize::from(line.ends_with('\n')) - usize::from(line.ends_with("\r\n"));
        lines.push(LineRange {
            start,
            content_end,
            end: line_end,
        });
        start = line_end;
    }
    if start == base || start < end {
        lines.push(LineRange {
            start,
            content_end: end,
            end,
        });
    }
    lines
}

#[derive(Debug, Default)]
struct MappedText {
    text: String,
    source_bytes: Vec<u32>,
    end: u32,
}

impl MappedText {
    fn direct(source: &str, range: TextRange) -> Self {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let mut mapped = Self::default();
        mapped.push_slice(source, start, end);
        mapped
    }

    fn push_slice(&mut self, source: &str, start: usize, end: usize) {
        let Some(slice) = source.get(start..end) else {
            return;
        };
        self.text.push_str(slice);
        self.source_bytes.extend(start as u32..end as u32);
        self.end = end as u32;
    }

    fn source_range(&self, start: usize, end: usize) -> Option<TextRange> {
        if start > end || end > self.source_bytes.len() {
            return None;
        }
        if start == end {
            let at = self.source_bytes.get(start).copied().unwrap_or(self.end);
            return Some(TextRange::empty(TextSize::new(at)));
        }
        Some(TextRange::new(
            TextSize::new(*self.source_bytes.get(start)?),
            TextSize::new(self.source_bytes.get(end - 1)?.checked_add(1)?),
        ))
    }
}
