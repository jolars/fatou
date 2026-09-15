//! Documentation-aware source routing shared by LSP features.
//!
//! Julia owns the outer document, but a static docstring contains a second,
//! decoded Markdown document, and some Markdown fences contain a third Julia
//! document. This module composes those coordinate spaces through the parser's
//! exact docstring source map. It never evaluates documentation metadata.

use fatou_parser::ast::{DocText, StaticDocText};
use fatou_parser::documentation::ParseOutput;
use fatou_parser::documentation::ast::{
    CodeBlock, DocumenterLinkKind, FenceKind, FootnoteReference, Link,
};
use rowan::ast::AstNode as _;
use rowan::{TextRange, TextSize};

use crate::documentation::DocumentationIndex;
pub(crate) use crate::documentation::{MarkdownReference, static_documentation};
use crate::incremental::{Analysis, SourceFile};
use crate::semantic::{SemanticDoc, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};
use std::sync::Arc;

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
    ///
    /// `index` indexes [`text`](Self::text); a caller mapping many ranges
    /// builds it once.
    pub(crate) fn source_range_from_lsp(
        &self,
        range: lsp_types::Range,
        index: &LineIndex<'_>,
        encoding: PositionEncoding,
    ) -> Option<TextRange> {
        let start = index.position_to_byte(range.start, encoding);
        let end = index.position_to_byte(range.end, encoding);
        self.source_range(TextRange::new(
            TextSize::new(start as u32),
            TextSize::new(end as u32),
        ))
    }
}

/// Demand the shared index only after the cursor selects Markdown navigation.
/// Fresh-parse and embedded-document callers build the same projection locally.
pub(crate) fn markdown_index(
    model: &SemanticModel,
    cached: Option<(&Analysis, SourceFile)>,
) -> Arc<DocumentationIndex> {
    match cached {
        Some((snapshot, file)) => snapshot.documentation_index(file).clone(),
        None => Arc::new(DocumentationIndex::build(
            static_documentation(model).map(|(_, decoded)| decoded.as_str()),
        )),
    }
}

/// Render a decoded documentation value as Markdown, omitting empty payloads.
pub(crate) fn render(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Convert an outer-source byte range to the negotiated LSP encoding.
///
/// Takes the index rather than the text: callers converting many ranges must
/// not rescan the document once per range.
pub(crate) fn lsp_range(
    range: TextRange,
    index: &LineIndex<'_>,
    encoding: PositionEncoding,
) -> lsp_types::Range {
    lsp_types::Range::new(
        index.byte_to_position(range.start().into(), encoding),
        index.byte_to_position(range.end().into(), encoding),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use crate::text::TextBuffer;

    #[test]
    fn cached_markdown_requests_follow_live_source_maps_and_fallbacks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("docs.jl");
        let uri = super::super::uri::from_path(&path).unwrap();
        let original = concat!(
            "x = 1\n",
            "\"\"\"\n    # First title\n\n",
            "    [Spot](@id chosen-id)\n\n",
            "    [^note]: First note.\n    \"\"\"\nf() = x\n",
            "\"\"\"\n# First title\n\n[Spot](@id chosen-id)\n\n",
            "[^note]: Second note.\n\"\"\"\ng() = 2\n",
            "\"\"\"\nSee [title](#first-title), [id](@ref chosen-id), and [^note].\n",
            "See [missing](#missing).\n\"\"\"\nh() = 3\n",
        );
        let mut db = IncrementalDatabase::new();
        for text in [
            original.to_string(),
            original.replace("    ", "        "),
            original.replace("x = 1", "# A new comment.\nx = 10000"),
            original.replace("First title", "\\u0046irst title"),
            format!("\"$opaque\"\nopaque() = 0\n{original}"),
            original.replacen("chosen-id", "changed-id", 1),
            original.replace("First title", "Other title"),
        ] {
            let live = TextBuffer::from(text.as_str());
            let index = live.line_index();
            let encoding = PositionEncoding::Utf16;
            // Exercise the missing/stale-buffer fallback before installing each
            // new revision, then demand the same answers twice from its cache.
            for pass in 0..3 {
                if pass == 1 {
                    db.upsert_file(&path, live.text_arc());
                }
                let snapshot = db.snapshot();
                for (marker, destination) in [
                    (
                        "#first-tit",
                        text.find("# First").or_else(|| text.find("# \\u0046irst")),
                    ),
                    ("@ref chosen-i", text.find("chosen-id)")),
                    ("and [^no", text.find("[^note]: First")),
                    ("#miss", None),
                ] {
                    let offset = text.rfind(marker).unwrap() + marker.len();
                    let position = index.byte_to_position(offset, encoding);
                    let actual = super::super::definition::definition_via_db(
                        &snapshot, &uri, &path, &live, position, encoding,
                    );
                    let fresh = super::super::definition::compute_definition(
                        &uri, &text, position, encoding, &snapshot,
                    );
                    assert_eq!(actual, fresh);
                    assert_eq!(actual.len(), usize::from(destination.is_some()));
                    if let Some(destination) = destination {
                        assert_eq!(
                            actual[0].range.start,
                            index.byte_to_position(destination, encoding)
                        );
                    }
                }
                let position = index.byte_to_position(
                    text.rfind("@ref chosen-i").unwrap() + "@ref chosen-i".len(),
                    encoding,
                );
                let actual = super::super::completion::completion_via_db(
                    &snapshot, &path, &live, position, encoding,
                );
                assert_eq!(
                    actual,
                    super::super::completion::compute_completions(
                        &text, position, encoding, &snapshot,
                    )
                );
                let anchors: Vec<_> = actual
                    .iter()
                    .filter(|item| item.kind == Some(lsp_types::CompletionItemKind::REFERENCE))
                    .map(|item| item.label.as_str())
                    .collect();
                assert_eq!(
                    anchors.iter().filter(|name| **name == "chosen-id").count(),
                    1
                );
                assert_eq!(
                    anchors.contains(&"First title"),
                    !text.contains("Other title")
                );
            }
        }
    }
}
