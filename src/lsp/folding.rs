//! Folding ranges (`textDocument/foldingRange`): Julia block constructs (plus
//! branch clauses), comment runs, import groups, and Markdown sections and
//! containers in static docstrings. Julia-bearing Markdown fences recursively
//! contribute their own code folds.
//!
//! Conventions: folds span the whole construct through its closing `end`
//! (rust-analyzer's convention — a collapsed function shows only its header
//! line), and are line-only (`start_character`/`end_character` unset): the
//! major clients fold whole lines anyway, and omitting the character fields
//! keeps the result independent of the negotiated position encoding.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{FoldingRange, FoldingRangeKind, Position, Range};
use rowan::ast::AstNode as _;

use crate::incremental::Analysis;
use crate::parser::parse;
use crate::semantic::SemanticModel;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use crate::text::{LineIndex, PositionEncoding, TextBuffer};
use fatou_parser::documentation::ast::{CodeBlock, Heading};
use fatou_parser::documentation::syntax::SyntaxKind as DocSyntaxKind;

/// The folding ranges for `text`, re-parsing it. Pure and unit-testable;
/// single-file by nature.
///
/// Best-effort, with no clean-parse gate: folds for the intact parts of a
/// broken buffer are still useful while the user types.
pub fn compute_folding_ranges(text: &str) -> Vec<FoldingRange> {
    let root = parse(text).cst;
    let model = SemanticModel::build(&root);
    folds_for_tree(&root, &model, text)
}

/// Compute folding ranges off the snapshot's cached parse when the db's
/// tracked buffer for `path` still matches `text`; otherwise re-parse. A write
/// racing the read trips `salsa::Cancelled`, which also falls back to a fresh
/// parse. Mirrors [`document_symbols_via_db`](super::symbols::document_symbols_via_db).
pub(crate) fn folding_ranges_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
) -> Vec<FoldingRange> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let model = snapshot.semantic_model(file);
        Some(folds_for_tree(&root, model, text))
    }));
    match cached {
        Ok(Some(folds)) => folds,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_folding_ranges(text),
    }
}

/// Shared entry point for the fresh-parse and cached-tree paths: `root` must be
/// the parse tree of exactly `text`.
fn folds_for_tree(root: &SyntaxNode, model: &SemanticModel, text: &str) -> Vec<FoldingRange> {
    let mut out = julia_folds(root, text);
    collect_documentation_folds(model, &Ctx::new(text), &mut out);
    out
}

/// The Julia-only folds of `root`, without the documentation pass. A Markdown
/// fence body reaches this directly: it needs no semantic model, and building
/// one per fence would re-decode every docstring the fence contains.
fn julia_folds(root: &SyntaxNode, text: &str) -> Vec<FoldingRange> {
    let ctx = Ctx {
        text,
        line_index: LineIndex::new(text),
    };
    let mut out = Vec::new();
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::MODULE_DEF
            | SyntaxKind::FUNCTION_DEF
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::STRUCT_DEF
            | SyntaxKind::ABSTRACT_DEF
            | SyntaxKind::PRIMITIVE_DEF
            | SyntaxKind::IF_EXPR
            | SyntaxKind::ELSEIF_CLAUSE
            | SyntaxKind::ELSE_CLAUSE
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::LET_EXPR
            | SyntaxKind::BEGIN_EXPR
            | SyntaxKind::QUOTE_EXPR
            | SyntaxKind::TYPEGROUP_DEF
            | SyntaxKind::TRY_EXPR
            | SyntaxKind::CATCH_CLAUSE
            | SyntaxKind::FINALLY_CLAUSE
            | SyntaxKind::DO_EXPR => ctx.push_fold(&mut out, node.text_range(), None),
            // A single statement spanning lines (`using Foo:` + a name list)
            // folds on its own; runs of statements group below.
            SyntaxKind::USING_STMT | SyntaxKind::IMPORT_STMT => {
                ctx.push_fold(&mut out, node.text_range(), Some(FoldingRangeKind::Imports));
            }
            _ => {}
        }
        collect_import_groups(&node, &ctx, &mut out);
    }
    collect_comment_folds(root, &ctx, &mut out);
    out
}

/// Fold Markdown sections and containers in every statically decoded docstring,
/// then add the ordinary Julia folds nested inside Julia-bearing fences.
fn collect_documentation_folds(model: &SemanticModel, ctx: &Ctx<'_>, out: &mut Vec<FoldingRange>) {
    for (_, decoded) in super::documentation::static_documentation(model) {
        let markdown = fatou_parser::documentation::parse(decoded.as_str());
        let headings: Vec<Heading> = markdown
            .cst
            .descendants()
            .filter_map(Heading::cast)
            .collect();
        let document_end = rowan::TextSize::new(decoded.as_str().len() as u32);
        for (index, heading) in headings.iter().enumerate() {
            let end = headings[index + 1..]
                .iter()
                .find(|next| next.level() <= heading.level())
                .map(|next| next.syntax().text_range().start())
                .unwrap_or(document_end);
            let range = rowan::TextRange::new(heading.syntax().text_range().start(), end);
            if let Some(source) = decoded.source_map().source_range(range) {
                ctx.push_fold(out, source, Some(FoldingRangeKind::Region));
            }
        }

        for node in markdown.cst.descendants() {
            if matches!(
                node.kind(),
                DocSyntaxKind::BLOCK_QUOTE
                    | DocSyntaxKind::ADMONITION
                    | DocSyntaxKind::LIST
                    | DocSyntaxKind::INDENTED_CODE_BLOCK
                    | DocSyntaxKind::FENCED_CODE_BLOCK
                    | DocSyntaxKind::MATH_BLOCK
                    | DocSyntaxKind::FOOTNOTE_DEFINITION
                    | DocSyntaxKind::TABLE
            ) && let Some(source) = decoded.source_map().source_range(node.text_range())
            {
                ctx.push_fold(out, source, Some(FoldingRangeKind::Region));
            }

            let Some(fence) = CodeBlock::cast(node) else {
                continue;
            };
            if !fence.fence_kind().contains_julia() {
                continue;
            }
            let Some(content_range) = fence.content_range() else {
                continue;
            };
            let start = usize::from(content_range.start());
            let end = usize::from(content_range.end());
            let Some(code) = decoded.as_str().get(start..end) else {
                continue;
            };
            let code_index = LineIndex::new(code);
            for fold in julia_folds(&parse(code).cst, code) {
                let relative = Range::new(
                    Position::new(fold.start_line, 0),
                    Position::new(fold.end_line.saturating_add(1), 0),
                );
                let relative_start =
                    code_index.position_to_byte(relative.start, PositionEncoding::Utf8);
                let relative_end =
                    code_index.position_to_byte(relative.end, PositionEncoding::Utf8);
                let base = u32::from(content_range.start());
                let decoded_range = rowan::TextRange::new(
                    rowan::TextSize::new(base + relative_start as u32),
                    rowan::TextSize::new(base + relative_end as u32),
                );
                if let Some(source) = decoded.source_map().source_range(decoded_range) {
                    ctx.push_fold(out, source, fold.kind);
                }
            }
        }
    }
}

struct Ctx<'a> {
    text: &'a str,
    line_index: LineIndex<'a>,
}

impl<'a> Ctx<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            line_index: LineIndex::new(text),
        }
    }
}

impl Ctx<'_> {
    /// The 0-indexed line containing the byte at `offset`. The encoding only
    /// affects the character column, which folding discards; UTF-8 skips the
    /// re-encoding.
    fn line_of(&self, offset: usize) -> u32 {
        self.line_index
            .byte_to_position(offset, PositionEncoding::Utf8)
            .line
    }

    /// The 0-indexed line containing the last byte of `range`. The end offset
    /// is exclusive, so an end at a line start (a trailing newline inside the
    /// node) belongs to the previous line.
    fn end_line(&self, range: rowan::TextRange) -> u32 {
        let pos = self
            .line_index
            .byte_to_position(range.end().into(), PositionEncoding::Utf8);
        if pos.character == 0 {
            pos.line.saturating_sub(1)
        } else {
            pos.line
        }
    }

    /// Emit a line-only fold over `range` if it spans more than one line.
    fn push_fold(
        &self,
        out: &mut Vec<FoldingRange>,
        range: rowan::TextRange,
        kind: Option<FoldingRangeKind>,
    ) {
        let start_line = self.line_of(range.start().into());
        let end_line = self.end_line(range);
        if end_line > start_line {
            out.push(FoldingRange {
                start_line,
                end_line,
                kind,
                ..Default::default()
            });
        }
    }

    /// Whether only whitespace precedes `token` on its line.
    fn leads_its_line(&self, token: &SyntaxToken) -> bool {
        let start = usize::from(token.text_range().start());
        let character = self
            .line_index
            .byte_to_position(start, PositionEncoding::Utf8)
            .character as usize;
        let line_start = start - character;
        self.text[line_start..start]
            .chars()
            .all(char::is_whitespace)
    }
}

/// Fold each run of two or more `using`/`import` statements among `node`'s
/// children where each starts on the line after the previous one ends. Any
/// intervening sibling breaks the run; a blank or comment line breaks the
/// line adjacency, which amounts to the same.
fn collect_import_groups(node: &SyntaxNode, ctx: &Ctx<'_>, out: &mut Vec<FoldingRange>) {
    // A run in progress: (start_line, end_line, statement count).
    let mut run: Option<(u32, u32, usize)> = None;
    for child in node.children() {
        if matches!(
            child.kind(),
            SyntaxKind::USING_STMT | SyntaxKind::IMPORT_STMT
        ) {
            let range = child.text_range();
            let start = ctx.line_of(range.start().into());
            let end = ctx.end_line(range);
            match &mut run {
                Some((_, run_end, count)) if start == *run_end + 1 => {
                    *run_end = end;
                    *count += 1;
                }
                _ => {
                    flush_import_run(run.take(), out);
                    run = Some((start, end, 1));
                }
            }
        } else {
            flush_import_run(run.take(), out);
        }
    }
    flush_import_run(run, out);
}

fn flush_import_run(run: Option<(u32, u32, usize)>, out: &mut Vec<FoldingRange>) {
    if let Some((start_line, end_line, count)) = run
        && count >= 2
        && end_line > start_line
    {
        out.push(FoldingRange {
            start_line,
            end_line,
            kind: Some(FoldingRangeKind::Imports),
            ..Default::default()
        });
    }
}

/// Fold multi-line block comments, and each run of two or more consecutive
/// whole-line `#` comments. A trailing comment after code neither starts nor
/// joins a run, and any line without a leading comment breaks adjacency.
fn collect_comment_folds(root: &SyntaxNode, ctx: &Ctx<'_>, out: &mut Vec<FoldingRange>) {
    // A run in progress: (start_line, last_line).
    let mut run: Option<(u32, u32)> = None;
    for token in root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        match token.kind() {
            SyntaxKind::BLOCK_COMMENT => {
                flush_comment_run(run.take(), out);
                ctx.push_fold(out, token.text_range(), Some(FoldingRangeKind::Comment));
            }
            SyntaxKind::COMMENT if ctx.leads_its_line(&token) => {
                let line = ctx.line_of(token.text_range().start().into());
                match &mut run {
                    Some((_, last)) if line == *last + 1 => *last = line,
                    Some(_) => {
                        flush_comment_run(run.take(), out);
                        run = Some((line, line));
                    }
                    None => run = Some((line, line)),
                }
            }
            _ => {}
        }
    }
    flush_comment_run(run, out);
}

fn flush_comment_run(run: Option<(u32, u32)>, out: &mut Vec<FoldingRange>) {
    if let Some((start_line, end_line)) = run
        && end_line > start_line
    {
        out.push(FoldingRange {
            start_line,
            end_line,
            kind: Some(FoldingRangeKind::Comment),
            ..Default::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back (still correctly) when the db
    /// lags the buffer or has never seen the path.
    #[test]
    fn folds_via_db_match_compute_and_fall_back() {
        let path = Path::new("/work/a.jl");
        let buffer = "function f(x)\n    x\nend\n";
        let expected = compute_folding_ranges(buffer);
        assert_eq!(expected.len(), 1, "fixture must yield a fold");

        // Cache hit: tracked text == buffer → folds off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        assert_eq!(
            folding_ranges_via_db(&db.snapshot(), path, &TextBuffer::new(buffer.to_string())),
            expected,
            "cached-tree folds must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            folding_ranges_via_db(
                &stale.snapshot(),
                path,
                &TextBuffer::new(buffer.to_string())
            ),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            folding_ranges_via_db(
                &empty.snapshot(),
                path,
                &TextBuffer::new(buffer.to_string())
            ),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }

    #[test]
    fn folds_markdown_sections_and_embedded_julia() {
        let source = concat!(
            "\"\"\"\n",
            "# Overview\n",
            "\n",
            "Intro.\n",
            "\n",
            "## Example\n",
            "\n",
            "```julia\n",
            "function example(x)\n",
            "    x\n",
            "end\n",
            "```\n",
            "\"\"\"\n",
            "f(x) = x\n",
        );
        let folds = compute_folding_ranges(source);
        let spans: Vec<_> = folds
            .iter()
            .map(|fold| (fold.start_line, fold.end_line))
            .collect();
        assert!(spans.contains(&(1, 11)), "overview section: {folds:?}");
        assert!(spans.contains(&(5, 11)), "example section: {folds:?}");
        assert!(spans.contains(&(7, 11)), "code fence: {folds:?}");
        assert!(spans.contains(&(8, 10)), "embedded function: {folds:?}");
    }
}
