//! Semantic tokens (`textDocument/semanticTokens/full`): syntax-driven
//! highlighting from a pure CST walk — keywords, macro names, string-macro
//! prefixes/suffixes, and literals. No name resolution; identifier
//! classification (function vs type vs module) arrives with the semantic
//! model (see `TODO.md`, language server Phase 6).
//!
//! Conventions: a macro name paints as one token over the sigil and the final
//! name component (`@show`; `@time` in `Base.@time`, the qualifier stays
//! plain), string delimiters and content coalesce into one string token per
//! line, string-macro prefixes and suffixes paint as macros (`r"x"` calls
//! `@r_str`; the suffix is an argument to it), and interpolations inside
//! strings stay unpainted so they render as code. Tokens never span line
//! breaks — most clients reject multiline semantic tokens — so multi-line
//! spans are split per line before encoding.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Position, SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend};
use rowan::{TextRange, TextSize};

use crate::incremental::Analysis;
use crate::parser::parse;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use crate::text::{LineIndex, PositionEncoding};

/// The token classes this server emits; the discriminant is the index into
/// [`legend`]'s `token_types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightKind {
    Keyword = 0,
    Macro = 1,
    String = 2,
    Number = 3,
}

/// The legend advertised in the server capabilities. Order must match
/// [`HighlightKind`]'s discriminants.
pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::MACRO,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
        ],
        token_modifiers: vec![],
    }
}

/// The semantic tokens for `text`, re-parsing it. Pure and unit-testable;
/// single-file by nature.
///
/// Best-effort, with no clean-parse gate: highlighting the intact parts of a
/// broken buffer is still useful while the user types.
pub fn compute_semantic_tokens(text: &str, encoding: PositionEncoding) -> SemanticTokens {
    let root = parse(text).cst;
    tokens_for_tree(&root, text, encoding)
}

/// Compute semantic tokens off the snapshot's cached parse when the db's
/// tracked buffer for `path` still matches `text`; otherwise re-parse. A write
/// racing the read trips `salsa::Cancelled`, which also falls back to a fresh
/// parse. Mirrors [`selection_ranges_via_db`](super::selection::selection_ranges_via_db).
pub(crate) fn semantic_tokens_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> SemanticTokens {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        Some(tokens_for_tree(&root, text, encoding))
    }));
    match cached {
        Ok(Some(tokens)) => tokens,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_semantic_tokens(text, encoding),
    }
}

/// Shared entry point for the fresh-parse and cached-tree paths: `root` must be
/// the parse tree of exactly `text`.
fn tokens_for_tree(root: &SyntaxNode, text: &str, encoding: PositionEncoding) -> SemanticTokens {
    let mut spans: Vec<(TextRange, HighlightKind)> = Vec::new();
    for token in root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        let Some(kind) = classify(&token) else {
            continue;
        };
        match spans.last_mut() {
            // Coalesce byte-adjacent same-kind spans: `@` + name become one
            // macro token, delimiters + content one string token.
            Some((range, last)) if *last == kind && range.end() == token.text_range().start() => {
                *range = TextRange::new(range.start(), token.text_range().end());
            }
            _ => spans.push((token.text_range(), kind)),
        }
    }
    SemanticTokens {
        result_id: None,
        data: delta_encode(&spans, text, encoding),
    }
}

/// The highlight class for a single token, if any. Structural rules (macro
/// names, string parts) go by the parent node; everything else by kind alone.
fn classify(token: &SyntaxToken) -> Option<HighlightKind> {
    match token.parent().map(|parent| parent.kind()) {
        Some(SyntaxKind::MACRO_NAME) => classify_in_macro_name(token),
        Some(SyntaxKind::STRING_LITERAL | SyntaxKind::CMD_LITERAL) => {
            classify_in_string(token.kind())
        }
        // `var"..."` bodies (NONSTANDARD_IDENTIFIER) fall through here and
        // stay plain: they are identifiers, not strings.
        _ => classify_by_kind(token.kind()),
    }
}

/// Inside a `MACRO_NAME`, paint the sigil and the final name component — the
/// `x` in `@A.B.x`, the operator in `@+`, the keyword in `@macro` — leaving
/// qualifiers plain until Phase 6 resolves namespaces. Trailing-sigil
/// qualifiers (`A.B.@x`) sit in nested nodes, so the parent gate already
/// excludes them; only the leading-sigil path components need skipping here.
fn classify_in_macro_name(token: &SyntaxToken) -> Option<HighlightKind> {
    if token.kind() == SyntaxKind::AT {
        return Some(HighlightKind::Macro);
    }
    // The `)` closing the parenthesized `@(expr)` form names nothing.
    if token.kind() != SyntaxKind::RPAREN && is_last_name_token(token) {
        return Some(HighlightKind::Macro);
    }
    None
}

/// Whether `token` is the last non-trivia token directly under its parent.
fn is_last_name_token(token: &SyntaxToken) -> bool {
    let Some(parent) = token.parent() else {
        return false;
    };
    parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !is_trivia(t.kind()))
        .last()
        .is_some_and(|last| last == *token)
}

/// Inside a string or command literal: delimiters and content are string;
/// the macro prefix and suffix flags are macros. A numeric string-macro
/// suffix (`x"1"2`) falls through to the kind rules and paints as a number.
fn classify_in_string(kind: SyntaxKind) -> Option<HighlightKind> {
    match kind {
        SyntaxKind::STRING_CONTENT
        | SyntaxKind::STRING_DELIM_OPEN
        | SyntaxKind::STRING_DELIM_CLOSE
        | SyntaxKind::CMD_DELIM_OPEN
        | SyntaxKind::CMD_DELIM_CLOSE => Some(HighlightKind::String),
        SyntaxKind::STRING_PREFIX | SyntaxKind::STRING_SUFFIX => Some(HighlightKind::Macro),
        _ => classify_by_kind(kind),
    }
}

/// Context-free classification: keywords and non-string literals.
fn classify_by_kind(kind: SyntaxKind) -> Option<HighlightKind> {
    if is_keyword(kind) {
        return Some(HighlightKind::Keyword);
    }
    match kind {
        SyntaxKind::CHAR => Some(HighlightKind::String),
        SyntaxKind::INTEGER
        | SyntaxKind::BIN_INT
        | SyntaxKind::OCT_INT
        | SyntaxKind::HEX_INT
        | SyntaxKind::FLOAT
        | SyntaxKind::FLOAT32 => Some(HighlightKind::Number),
        _ => None,
    }
}

/// All keyword tokens, `true`/`false` included: the standard legend has no
/// boolean type, and `keyword` matches the lexer's classification.
fn is_keyword(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FUNCTION_KW
            | SyntaxKind::MACRO_KW
            | SyntaxKind::END_KW
            | SyntaxKind::IF_KW
            | SyntaxKind::ELSEIF_KW
            | SyntaxKind::ELSE_KW
            | SyntaxKind::BEGIN_KW
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
            | SyntaxKind::WHILE_KW
            | SyntaxKind::FOR_KW
            | SyntaxKind::LET_KW
            | SyntaxKind::QUOTE_KW
            | SyntaxKind::TRY_KW
            | SyntaxKind::CATCH_KW
            | SyntaxKind::FINALLY_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::MUTABLE_KW
            | SyntaxKind::MODULE_KW
            | SyntaxKind::BAREMODULE_KW
            | SyntaxKind::DO_KW
            | SyntaxKind::RETURN_KW
            | SyntaxKind::BREAK_KW
            | SyntaxKind::CONTINUE_KW
            | SyntaxKind::CONST_KW
            | SyntaxKind::GLOBAL_KW
            | SyntaxKind::LOCAL_KW
            | SyntaxKind::IMPORT_KW
            | SyntaxKind::USING_KW
            | SyntaxKind::EXPORT_KW
            | SyntaxKind::WHERE_KW
    )
}

fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE)
}

/// Fold position-ordered spans into the LSP's relative encoding, splitting
/// multi-line spans first. `delta_start` and `length` count code units of the
/// negotiated encoding, which [`LineIndex::byte_to_position`] produces.
fn delta_encode(
    spans: &[(TextRange, HighlightKind)],
    text: &str,
    encoding: PositionEncoding,
) -> Vec<SemanticToken> {
    let line_index = LineIndex::new(text);
    let mut data = Vec::new();
    let mut prev = Position::new(0, 0);
    for &(range, kind) in spans {
        for segment in split_at_line_breaks(range, text) {
            let start = line_index.byte_to_position(segment.start().into(), encoding);
            let end = line_index.byte_to_position(segment.end().into(), encoding);
            debug_assert_eq!(start.line, end.line, "segments never span line breaks");
            let delta_line = start.line - prev.line;
            let delta_start = if delta_line == 0 {
                start.character - prev.character
            } else {
                start.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: end.character - start.character,
                token_type: kind as u32,
                token_modifiers_bitset: 0,
            });
            prev = start;
        }
    }
    data
}

/// Split `range` at line breaks into per-line, non-empty segments, excluding
/// the `\n` (and a preceding `\r`) itself.
fn split_at_line_breaks(range: TextRange, text: &str) -> Vec<TextRange> {
    let base = usize::from(range.start());
    let slice = &text[base..usize::from(range.end())];
    let mut segments = Vec::new();
    let mut push = |from: usize, to: usize| {
        let to = if slice.as_bytes()[from..to].last() == Some(&b'\r') {
            to - 1
        } else {
            to
        };
        if to > from {
            segments.push(TextRange::new(
                TextSize::new((base + from) as u32),
                TextSize::new((base + to) as u32),
            ));
        }
    };
    let mut start = 0;
    for (i, byte) in slice.bytes().enumerate() {
        if byte == b'\n' {
            push(start, i);
            start = i + 1;
        }
    }
    push(start, slice.len());
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;

    /// The legend's order is the contract between [`HighlightKind`]'s
    /// discriminants and the indices on the wire.
    #[test]
    fn legend_order_matches_highlight_kind_discriminants() {
        let legend = legend();
        for (kind, token_type) in [
            (HighlightKind::Keyword, SemanticTokenType::KEYWORD),
            (HighlightKind::Macro, SemanticTokenType::MACRO),
            (HighlightKind::String, SemanticTokenType::STRING),
            (HighlightKind::Number, SemanticTokenType::NUMBER),
        ] {
            assert_eq!(legend.token_types[kind as usize], token_type);
        }
        assert_eq!(legend.token_types.len(), 4, "every kind is in the legend");
        assert!(legend.token_modifiers.is_empty());
    }

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back (still correctly) when the db
    /// lags the buffer or has never seen the path.
    #[test]
    fn semantic_tokens_via_db_match_compute_and_fall_back() {
        let path = Path::new("/work/a.jl");
        let buffer = "function f(x)\n    @show x + 1\nend\n";
        let expected = compute_semantic_tokens(buffer, PositionEncoding::Utf8);
        assert!(!expected.data.is_empty(), "fixture must yield tokens");

        // Cache hit: tracked text == buffer → tokens off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        assert_eq!(
            semantic_tokens_via_db(&db.snapshot(), path, buffer, PositionEncoding::Utf8),
            expected,
            "cached-tree tokens must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            semantic_tokens_via_db(&stale.snapshot(), path, buffer, PositionEncoding::Utf8),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            semantic_tokens_via_db(&empty.snapshot(), path, buffer, PositionEncoding::Utf8),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
