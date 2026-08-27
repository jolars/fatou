//! Documentation-aware source routing shared by LSP features.
//!
//! Julia owns the outer document, but a static docstring contains a second,
//! decoded Markdown document, and some Markdown fences contain a third Julia
//! document. This module composes those coordinate spaces through the parser's
//! exact docstring source map. It never evaluates documentation metadata.

use fatou_parser::ast::{DocText, StaticDocText};
use fatou_parser::documentation::ParseOutput;
use fatou_parser::documentation::ast::{
    CodeBlock, DocumenterLinkKind, FenceKind, FootnoteDefinition, FootnoteReference, Heading, Link,
};
use rowan::ast::AstNode as _;
use rowan::{TextRange, TextSize};

use crate::semantic::{SemanticDoc, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

/// A static documentation attachment containing the source cursor.
pub(crate) struct DocumentationContext<'a> {
    pub(crate) attachment: &'a SemanticDoc,
    pub(crate) decoded: &'a StaticDocText,
    pub(crate) markdown: ParseOutput,
    pub(crate) offset: TextSize,
}

impl<'a> DocumentationContext<'a> {
    /// Find the decoded documentation position corresponding to `source_offset`.
    pub(crate) fn at(model: &'a SemanticModel, source_offset: TextSize) -> Option<Self> {
        model.documentation().iter().find_map(|attachment| {
            let DocText::Static(decoded) = &attachment.text else {
                return None;
            };
            let offset = decoded.source_map().decoded_offset(source_offset)?;
            Some(Self {
                attachment,
                decoded,
                markdown: fatou_parser::documentation::parse(decoded.as_str()),
                offset,
            })
        })
    }

    /// The explicit target of an `@ref` link under the cursor.
    pub(crate) fn explicit_ref(&self) -> Option<ExplicitRef> {
        if let Some(reference) = self.markdown.cst.descendants().find_map(|node| {
            let link = Link::cast(node)?;
            let documenter = link.documenter_link()?;
            if documenter.kind() != DocumenterLinkKind::Ref {
                return None;
            }
            let range = documenter.target_range()?;
            range.contains_inclusive(self.offset).then(|| ExplicitRef {
                target: documenter.target().unwrap_or_default().to_string(),
                range,
                offset: TextSize::new(u32::from(self.offset) - u32::from(range.start())),
            })
        }) {
            return Some(reference);
        }

        // Completion commonly runs before the closing `)` exists. The
        // Markdown parser deliberately keeps malformed links as text, so use a
        // line-bounded recovery scan for that one partial shape.
        let cursor = usize::from(self.offset);
        let prefix = self.decoded.as_str().get(..cursor)?;
        let line_start = prefix.rfind(['\r', '\n']).map_or(0, |index| index + 1);
        let marker = prefix[line_start..].rfind("(@ref")? + line_start;
        let after_marker = marker + "(@ref".len();
        let suffix = &prefix[after_marker..];
        if suffix.is_empty() || !suffix.starts_with(char::is_whitespace) {
            return None;
        }
        let mut nested_parentheses = 0_u32;
        for character in suffix.chars() {
            match character {
                '(' => nested_parentheses += 1,
                ')' if nested_parentheses == 0 => return None,
                ')' => nested_parentheses -= 1,
                _ => {}
            }
        }
        let target_start = after_marker + suffix.len() - suffix.trim_start().len();
        let range = TextRange::new(TextSize::new(target_start as u32), self.offset);
        Some(ExplicitRef {
            target: prefix[target_start..].to_string(),
            range,
            offset: TextSize::new((cursor - target_start) as u32),
        })
    }

    /// A standard Markdown fragment or footnote reference under the cursor.
    pub(crate) fn markdown_reference(&self) -> Option<MarkdownReference> {
        if let Some(reference) = self.markdown.cst.descendants().find_map(|node| {
            let link = Link::cast(node)?;
            let destination = link.destination();
            let id = destination.strip_prefix('#')?;
            link.destination_range()?
                .contains_inclusive(self.offset)
                .then(|| MarkdownReference::Anchor(id.to_string()))
        }) {
            return Some(reference);
        }
        self.markdown.cst.descendants().find_map(|node| {
            let reference = FootnoteReference::cast(node)?;
            reference
                .syntax()
                .text_range()
                .contains_inclusive(self.offset)
                .then(|| MarkdownReference::Footnote(reference.id()))
        })
    }

    /// The Julia-bearing fenced block under the cursor.
    pub(crate) fn embedded_julia(&self) -> Option<EmbeddedJulia<'a>> {
        self.markdown.cst.descendants().find_map(|node| {
            let fence = CodeBlock::cast(node)?;
            if !fence.fence_kind().contains_julia() {
                return None;
            }
            let range = fence.content_range()?;
            if !range.contains_inclusive(self.offset) {
                return None;
            }
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            let text = self.decoded.as_str().get(start..end)?;
            Some(EmbeddedJulia {
                decoded: self.decoded,
                text,
                range,
                offset: TextSize::new(u32::from(self.offset) - u32::from(range.start())),
                kind: fence.fence_kind(),
            })
        })
    }

    /// Map a decoded Markdown range back into the Julia source.
    pub(crate) fn source_range(&self, range: TextRange) -> Option<TextRange> {
        self.decoded.source_map().source_range(range)
    }
}

/// One explicit `@ref target` destination and the cursor within it.
pub(crate) struct ExplicitRef {
    pub(crate) target: String,
    pub(crate) range: TextRange,
    pub(crate) offset: TextSize,
}

pub(crate) enum MarkdownReference {
    Anchor(String),
    Footnote(String),
}

/// A Julia subdocument declared by a Markdown fence.
pub(crate) struct EmbeddedJulia<'a> {
    decoded: &'a StaticDocText,
    pub(crate) text: &'a str,
    pub(crate) range: TextRange,
    pub(crate) offset: TextSize,
    #[allow(dead_code)]
    pub(crate) kind: FenceKind,
}

impl EmbeddedJulia<'_> {
    /// Map a range relative to the fenced Julia body into the outer Julia file.
    pub(crate) fn source_range(&self, range: TextRange) -> Option<TextRange> {
        let base = u32::from(self.range.start());
        let decoded = TextRange::new(
            TextSize::new(base.checked_add(u32::from(range.start()))?),
            TextSize::new(base.checked_add(u32::from(range.end()))?),
        );
        self.decoded.source_map().source_range(decoded)
    }

    /// Map an LSP range in the fenced body into an outer-source byte range.
    pub(crate) fn source_range_from_lsp(
        &self,
        range: lsp_types::Range,
        encoding: PositionEncoding,
    ) -> Option<TextRange> {
        let index = LineIndex::new(self.text);
        let start = index.position_to_byte(range.start, encoding);
        let end = index.position_to_byte(range.end, encoding);
        self.source_range(TextRange::new(
            TextSize::new(start as u32),
            TextSize::new(end as u32),
        ))
    }
}

/// Iterate the statically decoded documentation attachments in source order.
pub(crate) fn static_documentation(
    model: &SemanticModel,
) -> impl Iterator<Item = (&SemanticDoc, &StaticDocText)> {
    model
        .documentation()
        .iter()
        .filter_map(|doc| match &doc.text {
            DocText::Static(text) => Some((doc, text)),
            DocText::Opaque(_) | DocText::Invalid(_) => None,
        })
}

/// Find the source definition of a Markdown anchor or footnote in any static
/// docstring in the current Julia file.
pub(crate) fn markdown_definition(
    model: &SemanticModel,
    reference: &MarkdownReference,
) -> Option<TextRange> {
    for (_, decoded) in static_documentation(model) {
        let markdown = fatou_parser::documentation::parse(decoded.as_str());
        let decoded_range = match reference {
            MarkdownReference::Anchor(id) => markdown.cst.descendants().find_map(|node| {
                if let Some(link) = Link::cast(node.clone())
                    && let Some(documenter) = link.documenter_link()
                    && documenter.kind() == DocumenterLinkKind::Id
                    && documenter.target() == Some(id.as_str())
                {
                    return documenter.target_range();
                }
                let heading = Heading::cast(node)?;
                let content = heading.content();
                (content == *id || heading_slug(&content) == *id)
                    .then(|| heading.syntax().text_range())
            }),
            MarkdownReference::Footnote(id) => markdown
                .cst
                .descendants()
                .filter_map(FootnoteDefinition::cast)
                .find(|definition| definition.id() == *id)
                .map(|definition| definition.syntax().text_range()),
        };
        if let Some(range) = decoded_range
            && let Some(source) = decoded.source_map().source_range(range)
        {
            return Some(source);
        }
    }
    None
}

/// The explicit anchor names completion may offer inside `@ref`.
pub(crate) fn markdown_anchor_names(model: &SemanticModel) -> Vec<String> {
    let mut names = Vec::new();
    for (_, decoded) in static_documentation(model) {
        let markdown = fatou_parser::documentation::parse(decoded.as_str());
        for node in markdown.cst.descendants() {
            if let Some(link) = Link::cast(node.clone())
                && let Some(documenter) = link.documenter_link()
                && documenter.kind() == DocumenterLinkKind::Id
                && let Some(target) = documenter.target()
                && !names.iter().any(|name| name == target)
            {
                names.push(target.to_string());
            }
            if let Some(heading) = Heading::cast(node) {
                let content = heading.content();
                if !content.is_empty() && !names.contains(&content) {
                    names.push(content.clone());
                }
                let slug = heading_slug(&content);
                if !slug.is_empty() && !names.contains(&slug) {
                    names.push(slug);
                }
            }
        }
    }
    names
}

fn heading_slug(heading: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for character in heading.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            if separator && !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            out.push(character);
            separator = false;
        } else if character.is_whitespace() {
            separator = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Render a decoded documentation value as Markdown, omitting empty payloads.
pub(crate) fn render(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Convert an outer-source byte range to the negotiated LSP encoding.
pub(crate) fn lsp_range(
    range: TextRange,
    source: &str,
    encoding: PositionEncoding,
) -> lsp_types::Range {
    let index = LineIndex::new(source);
    lsp_types::Range::new(
        index.byte_to_position(range.start().into(), encoding),
        index.byte_to_position(range.end().into(), encoding),
    )
}
