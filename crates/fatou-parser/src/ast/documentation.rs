//! Typed documentation attachments and statically recoverable textual payloads.
//!
//! Julia's `@doc` accepts arbitrary metadata. This module therefore recognizes
//! attachment shape independently from content and decodes only standard
//! ordinary or `raw` string literals whose value is statically knowable.

use rowan::ast::AstNode as _;
use rowan::{TextRange, TextSize};

use super::{Doc, MacroCall, StringLiteral};
use crate::parser::StringDecodeError;
use crate::syntax::{SyntaxKind, SyntaxNode};

/// Whether documentation used Julia's adjacent-string sugar or an explicit
/// two-argument `@doc` macro call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAttachmentKind {
    Juxtaposed,
    Macro,
}

/// A documentable expression or statement.
///
/// Julia permits unrelated syntax kinds here, so this wrapper exposes the
/// lossless node without claiming that every target is an [`Expr`](super::Expr).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocTarget(SyntaxNode);

impl DocTarget {
    pub(crate) fn new(syntax: SyntaxNode) -> Self {
        Self(syntax)
    }

    /// Return the lossless syntax node for the documented target.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.0
    }
}

/// A documentation payload and the syntax it documents.
///
/// This is deliberately not an [`AstNode`](rowan::ast::AstNode): it unifies a
/// `DOC` node with qualified or unqualified spellings of a two-argument `@doc`
/// `MACRO_CALL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocAttachment {
    syntax: SyntaxNode,
    payload: SyntaxNode,
    target: SyntaxNode,
    kind: DocAttachmentKind,
}

impl DocAttachment {
    /// Recognize an adjacent docstring or exactly two arguments to a macro whose
    /// final identifier is `doc` (`@doc`, `Base.@doc`, or `@Base.doc`).
    pub fn cast(syntax: SyntaxNode) -> Option<Self> {
        if let Some(doc) = Doc::cast(syntax.clone()) {
            return Some(Self {
                payload: doc.literal()?.syntax().clone(),
                target: doc.target()?.syntax().clone(),
                syntax,
                kind: DocAttachmentKind::Juxtaposed,
            });
        }

        let call = MacroCall::cast(syntax.clone())?;
        let name = call.name()?;
        if name.macro_token()?.text() != "doc" {
            return None;
        }
        let args = macro_args(call.syntax());
        let [payload, target] = args.as_slice() else {
            return None;
        };
        Some(Self {
            syntax,
            payload: payload.clone(),
            target: target.clone(),
            kind: DocAttachmentKind::Macro,
        })
    }

    /// Return the complete attachment node.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.syntax
    }

    /// Return the metadata expression passed as documentation.
    pub fn payload(&self) -> &SyntaxNode {
        &self.payload
    }

    /// Return the expression or statement that receives the documentation.
    pub fn target(&self) -> &SyntaxNode {
        &self.target
    }

    /// Return the Julia syntax form used for the attachment.
    pub fn kind(&self) -> DocAttachmentKind {
        self.kind
    }

    /// Recover textual documentation when Julia's value is statically known.
    pub fn text(&self) -> DocText {
        let Some(literal) = StringLiteral::cast(self.payload.clone()) else {
            return DocText::Opaque(OpaqueDocReason::NonString);
        };
        decode_literal(&literal)
    }
}

/// Why a documentation payload cannot be interpreted statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueDocReason {
    /// The payload is not a string literal.
    NonString,
    /// The string literal contains interpolation.
    Interpolation,
    /// The string macro is not Julia's standard `raw` string.
    UnsupportedPrefix,
    /// The literal has a suffix, so a string macro controls its meaning.
    UnsupportedSuffix,
}

/// The statically knowable status of a documentation payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocText {
    /// The exact documentation value is statically recoverable.
    Static(StaticDocText),
    /// Julia syntax permits the payload, but its value requires evaluation or
    /// string-macro semantics.
    Opaque(OpaqueDocReason),
    /// A standard string literal contains an invalid escape or byte sequence.
    Invalid(StringDecodeError),
}

/// Decoded documentation text and its mapping back to Julia source bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDocText {
    text: String,
    source_map: DocSourceMap,
}

impl StaticDocText {
    /// Return the decoded documentation value.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Return the mapping from decoded bytes to Julia source bytes.
    pub fn source_map(&self) -> &DocSourceMap {
        &self.source_map
    }
}

/// An exact byte-span map from decoded documentation to the string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSourceMap {
    bytes: Vec<SourceSpan>,
    empty_at: TextSize,
}

impl DocSourceMap {
    /// Map a decoded UTF-8 byte range to its enclosing absolute source range.
    /// Returns `None` for an out-of-bounds range.
    pub fn source_range(&self, range: TextRange) -> Option<TextRange> {
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        if start > end || end > self.bytes.len() {
            return None;
        }
        if start == end {
            let at = self
                .bytes
                .get(start)
                .map(|span| span.start)
                .or_else(|| self.bytes.last().map(|span| span.end))
                .unwrap_or(self.empty_at);
            return Some(TextRange::new(at, at));
        }
        Some(TextRange::new(
            self.bytes[start].start,
            self.bytes[end - 1].end,
        ))
    }

    /// Map an absolute Julia source offset into the decoded documentation.
    ///
    /// A cursor anywhere inside an escape maps to the first decoded byte that
    /// escape produced. Bytes removed by triple-string dedenting or a line
    /// continuation have no decoded position and return `None`.
    pub fn decoded_offset(&self, offset: TextSize) -> Option<TextSize> {
        if self.bytes.is_empty() {
            return (offset == self.empty_at).then_some(TextSize::new(0));
        }
        if offset == self.bytes.last()?.end {
            return Some(TextSize::new(self.bytes.len() as u32));
        }
        let index = self
            .bytes
            .iter()
            .position(|span| span.start <= offset && offset < span.end)?;
        let span = self.bytes[index];
        let first = self.bytes[..index]
            .iter()
            .rposition(|candidate| *candidate != span)
            .map_or(0, |previous| previous + 1);
        Some(TextSize::new(first as u32))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceSpan {
    start: TextSize,
    end: TextSize,
}

#[derive(Clone)]
struct MappedText {
    text: String,
    bytes: Vec<SourceSpan>,
    empty_at: TextSize,
}

impl MappedText {
    fn from_literal(literal: &StringLiteral) -> Self {
        let empty_at = literal
            .syntax()
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| token.kind() == SyntaxKind::STRING_DELIM_OPEN)
            .map(|token| token.text_range().end())
            .unwrap_or_else(|| literal.syntax().text_range().start());
        let mut out = Self {
            text: String::new(),
            bytes: Vec::new(),
            empty_at,
        };
        for token in literal.content_tokens() {
            let base: u32 = token.text_range().start().into();
            out.text.push_str(token.text());
            for i in 0..token.text().len() as u32 {
                out.bytes.push(SourceSpan {
                    start: TextSize::from(base + i),
                    end: TextSize::from(base + i + 1),
                });
            }
        }
        out
    }

    fn push_slice(&mut self, source: &Self, range: std::ops::Range<usize>) {
        self.text.push_str(&source.text[range.clone()]);
        self.bytes.extend_from_slice(&source.bytes[range]);
    }

    fn into_static(self) -> StaticDocText {
        StaticDocText {
            text: self.text,
            source_map: DocSourceMap {
                bytes: self.bytes,
                empty_at: self.empty_at,
            },
        }
    }
}

fn macro_args(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    for child in node.children() {
        match child.kind() {
            SyntaxKind::MACRO_NAME => {}
            SyntaxKind::ARG_LIST => {
                for arg in child.children() {
                    if arg.kind() == SyntaxKind::ARG {
                        out.extend(arg.children());
                    } else {
                        out.push(arg);
                    }
                }
            }
            _ => out.push(child),
        }
    }
    out
}

fn decode_literal(literal: &StringLiteral) -> DocText {
    if literal.interpolations().next().is_some() {
        return DocText::Opaque(OpaqueDocReason::Interpolation);
    }
    if literal.suffix().is_some() {
        return DocText::Opaque(OpaqueDocReason::UnsupportedSuffix);
    }
    let raw = match literal.prefix().as_ref().map(|token| token.text()) {
        None => false,
        Some("raw") => true,
        Some(_) => return DocText::Opaque(OpaqueDocReason::UnsupportedPrefix),
    };
    let triple = literal
        .syntax()
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::STRING_DELIM_OPEN)
        .is_some_and(|token| token.text().len() >= 3);

    let mut mapped = MappedText::from_literal(literal);
    if raw {
        mapped = decode_raw(mapped);
    }
    if triple {
        mapped = dedent_triple(normalize_newlines(mapped));
    }
    if !raw {
        mapped = match decode_escapes(mapped) {
            Ok(mapped) => mapped,
            Err(err) => return DocText::Invalid(err),
        };
    }
    DocText::Static(mapped.into_static())
}

fn normalize_newlines(input: MappedText) -> MappedText {
    let bytes = input.text.as_bytes();
    let mut out = MappedText {
        text: String::new(),
        bytes: Vec::new(),
        empty_at: input.empty_at,
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            let end = if bytes.get(i + 1) == Some(&b'\n') {
                i + 2
            } else {
                i + 1
            };
            out.text.push('\n');
            out.bytes.push(SourceSpan {
                start: input.bytes[i].start,
                end: input.bytes[end - 1].end,
            });
            i = end;
        } else {
            let len = input.text[i..].chars().next().unwrap().len_utf8();
            out.push_slice(&input, i..i + len);
            i += len;
        }
    }
    out
}

fn dedent_triple(input: MappedText) -> MappedText {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, byte) in input.text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push((start, i, Some(i)));
            start = i + 1;
        }
    }
    lines.push((start, input.text.len(), None));

    let last = lines.len() - 1;
    let candidates: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, (start, end, _))| {
            *i != 0
                && (*i == last
                    || !input.text[*start..*end]
                        .bytes()
                        .all(|byte| matches!(byte, b' ' | b'\t')))
        })
        .map(|(_, (start, end, _))| {
            let line = &input.text[*start..*end];
            &line[..line
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count()]
        })
        .collect();
    let dedent = common_whitespace_prefix_len(&candidates);

    let mut out = MappedText {
        text: String::new(),
        bytes: Vec::new(),
        empty_at: input.empty_at,
    };
    for (i, (start, end, newline)) in lines.into_iter().enumerate() {
        if i == 0 && start == end {
            continue;
        }
        let strip = if i == 0 { 0 } else { dedent.min(end - start) };
        out.push_slice(&input, start + strip..end);
        if let Some(newline) = newline {
            out.push_slice(&input, newline..newline + 1);
        }
    }
    out
}

/// Return the common byte prefix of leading space/tab strings.
///
/// This is shared with the JuliaSyntax projector so the public docstring value
/// and the differential oracle cannot drift on triple-string indentation.
pub(crate) fn common_whitespace_prefix_len(strings: &[&str]) -> usize {
    let Some(first) = strings.first() else {
        return 0;
    };
    strings.iter().skip(1).fold(first.len(), |len, string| {
        first
            .bytes()
            .zip(string.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count()
    })
}

fn decode_raw(input: MappedText) -> MappedText {
    let bytes = input.text.as_bytes();
    let mut out = MappedText {
        text: String::new(),
        bytes: Vec::new(),
        empty_at: input.empty_at,
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            let len = input.text[i..].chars().next().unwrap().len_utf8();
            out.push_slice(&input, i..i + len);
            i += len;
            continue;
        }
        let mut run = 0;
        while i + run < bytes.len() && bytes[i + run] == b'\\' {
            run += 1;
        }
        let at_end = i + run == bytes.len();
        let before_quote = bytes.get(i + run) == Some(&b'"');
        if !at_end && !before_quote {
            out.push_slice(&input, i..i + run);
            i += run;
            continue;
        }
        for pair in 0..run / 2 {
            let start = i + pair * 2;
            out.text.push('\\');
            out.bytes.push(SourceSpan {
                start: input.bytes[start].start,
                end: input.bytes[start + 1].end,
            });
        }
        i += run;
        if run % 2 == 1 {
            out.text.push('"');
            let end = if before_quote { i + 1 } else { i };
            out.bytes.push(SourceSpan {
                start: input.bytes[i - 1].start,
                end: input.bytes[end.saturating_sub(1)].end,
            });
            i = end;
        }
    }
    out
}

fn decode_escapes(input: MappedText) -> Result<MappedText, StringDecodeError> {
    let source = input.text.as_bytes();
    let mut decoded = Vec::new();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < source.len() {
        if source[i] != b'\\' {
            let len = input.text[i..].chars().next().unwrap().len_utf8();
            decoded.extend_from_slice(&source[i..i + len]);
            spans.extend_from_slice(&input.bytes[i..i + len]);
            i += len;
            continue;
        }
        let start = i;
        i += 1;
        let Some(&escaped) = source.get(i) else {
            return Err(StringDecodeError::BadEscape);
        };
        if matches!(escaped, b'\n' | b'\r') {
            if escaped == b'\r' && source.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            i += 1;
            while matches!(source.get(i), Some(b' ' | b'\t')) {
                i += 1;
            }
            continue;
        }
        let value = match escaped {
            b'n' => vec![b'\n'],
            b't' => vec![b'\t'],
            b'r' => vec![b'\r'],
            b'a' => vec![0x07],
            b'b' => vec![0x08],
            b'f' => vec![0x0c],
            b'v' => vec![0x0b],
            b'e' => vec![0x1b],
            b'\\' => vec![b'\\'],
            b'\'' => vec![b'\''],
            b'"' => vec![b'"'],
            b'$' => vec![b'$'],
            b'x' => decode_numeric_escape(source, &mut i, 2, false)?,
            b'u' => decode_numeric_escape(source, &mut i, 4, true)?,
            b'U' => decode_numeric_escape(source, &mut i, 8, true)?,
            b'0'..=b'7' => decode_octal_escape(source, &mut i)?,
            _ => return Err(StringDecodeError::BadEscape),
        };
        i += 1;
        let span = SourceSpan {
            start: input.bytes[start].start,
            end: input.bytes[i - 1].end,
        };
        decoded.extend_from_slice(&value);
        spans.extend(std::iter::repeat_n(span, value.len()));
    }
    let text = String::from_utf8(decoded).map_err(|_| StringDecodeError::BadUtf8)?;
    Ok(MappedText {
        text,
        bytes: spans,
        empty_at: input.empty_at,
    })
}

fn decode_numeric_escape(
    source: &[u8],
    i: &mut usize,
    max: usize,
    unicode: bool,
) -> Result<Vec<u8>, StringDecodeError> {
    let mut value = 0u32;
    let mut count = 0;
    while count < max {
        let Some(digit) = source
            .get(*i + 1)
            .and_then(|byte| char::from(*byte).to_digit(16))
        else {
            break;
        };
        *i += 1;
        value = value * 16 + digit;
        count += 1;
    }
    if count == 0 {
        return Err(StringDecodeError::BadEscape);
    }
    if !unicode {
        return Ok(vec![value as u8]);
    }
    let character = char::from_u32(value).ok_or(StringDecodeError::BadEscape)?;
    let mut buffer = [0; 4];
    Ok(character.encode_utf8(&mut buffer).as_bytes().to_vec())
}

fn decode_octal_escape(source: &[u8], i: &mut usize) -> Result<Vec<u8>, StringDecodeError> {
    let mut value = u32::from(source[*i] - b'0');
    for _ in 0..2 {
        let Some(digit) = source
            .get(*i + 1)
            .and_then(|byte| char::from(*byte).to_digit(8))
        else {
            break;
        };
        *i += 1;
        value = value * 8 + digit;
    }
    let byte = u8::try_from(value).map_err(|_| StringDecodeError::BadEscape)?;
    Ok(vec![byte])
}
