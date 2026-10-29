use rowan::{GreenNodeBuilder, Language as _, TextRange, TextSize};

use super::syntax::{DocumentationLanguage, SyntaxKind, SyntaxNode};

/// A recoverable documentation parse problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub kind: DiagnosticKind,
    pub range: TextRange,
}

/// Kinds of malformed explicit Markdown syntax reported by the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// A backtick or tilde fence opened but did not close.
    UnclosedFence,
}

/// A lossless documentation CST and its recoverable diagnostics.
#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub cst: SyntaxNode,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Parse decoded documentation text as Julia-flavored Markdown.
pub fn parse(text: &str) -> ParseOutput {
    assert!(
        text.len() <= u32::MAX as usize,
        "documentation exceeds Rowan's 4 GiB limit"
    );
    Parser::new(text).parse()
}

/// Reconstruct the exact parser input from a documentation CST.
pub fn reconstruct(root: &SyntaxNode) -> String {
    root.text().to_string()
}

#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    content_end: usize,
    end: usize,
}

impl Line {
    fn content(self, text: &str) -> &str {
        &text[self.start..self.content_end]
    }

    fn is_blank(self, text: &str) -> bool {
        self.content(text).trim_matches([' ', '\t']).is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
struct FenceOpen {
    marker_start: usize,
    marker_end: usize,
    marker: u8,
    length: usize,
}

#[derive(Debug, Clone, Copy)]
enum FencePrefix {
    Spaces(usize),
    IndentedContainer,
}

#[derive(Clone, Copy)]
struct InlineLine {
    line: Line,
    content_start: usize,
    quote_marker: Option<usize>,
}

impl InlineLine {
    fn new(line: Line, content_start: usize) -> Self {
        Self {
            line,
            content_start,
            quote_marker: None,
        }
    }
}

struct InlinePiece {
    logical: std::ops::Range<usize>,
    source: std::ops::Range<usize>,
    kind: InlinePieceKind,
}

enum InlinePieceKind {
    Content,
    SoftBreak,
    HardBreak,
    Prefix(SyntaxKind),
}

struct Parser<'a> {
    text: &'a str,
    lines: Vec<Line>,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<ParseDiagnostic>,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            lines: lines(text),
            builder: GreenNodeBuilder::new(),
            diagnostics: Vec::new(),
        }
    }

    fn parse(mut self) -> ParseOutput {
        self.start(SyntaxKind::ROOT);
        let mut line = 0;
        while line < self.lines.len() {
            if self.lines[line].is_blank(self.text) {
                let current = self.lines[line];
                self.token(SyntaxKind::BLANK_LINE, current.start, current.end);
                line += 1;
                continue;
            }
            line = self.parse_block(line);
        }
        self.finish();
        let green = self.builder.finish();
        ParseOutput {
            cst: SyntaxNode::new_root(green),
            diagnostics: self.diagnostics,
        }
    }

    fn parse_block(&mut self, line: usize) -> usize {
        if let Some(expression_end) = block_interpolation(self.lines[line], self.text) {
            return self.emit_block_interpolation(line, expression_end);
        }
        if let Some(open) = fence_open(self.lines[line], self.text) {
            if let Some(close) = self.find_fence_close(line + 1, open, FencePrefix::Spaces(0)) {
                return self.emit_fence(
                    line,
                    close,
                    open,
                    FencePrefix::Spaces(0),
                    self.lines[line].start,
                );
            }
            self.diagnostics.push(ParseDiagnostic {
                kind: DiagnosticKind::UnclosedFence,
                range: range(open.marker_start, open.marker_end),
            });
            return self.emit_paragraph(line);
        }
        if atx_heading(self.lines[line], self.text).is_some() {
            return self.emit_atx_heading(line);
        }
        if line + 1 < self.lines.len()
            && contains_unescaped_pipe(self.lines[line].content(self.text))
            && table_alignment(self.lines[line + 1].content(self.text))
        {
            return self.emit_table(line);
        }
        if block_quote_content(self.lines[line], self.text).is_some() {
            return self.emit_block_quote(line);
        }
        if admonition_header(self.lines[line], self.text).is_some() {
            return self.emit_admonition(line);
        }
        if footnote_header(self.lines[line], self.text).is_some() {
            return self.emit_footnote(line);
        }
        if list_marker(self.lines[line], self.text).is_some() {
            return self.emit_list(line);
        }
        if indentation(self.lines[line].content(self.text)) >= 4
            || self.lines[line].content(self.text).starts_with('\t')
        {
            return self.emit_indented_code(line);
        }
        if thematic_break(self.lines[line].content(self.text)) {
            return self.emit_simple_line(line, SyntaxKind::THEMATIC_BREAK);
        }
        if math_block(self.lines[line], self.text).is_some() {
            return self.emit_math_block(line);
        }
        // Last before the paragraph fallback, as in Julia's flavor: an
        // underline only makes a heading out of a line no other block claimed,
        // so `- item` or `> quoted` above `---` stays a list or a quote
        // followed by a thematic break.
        if line + 1 < self.lines.len()
            && !self.lines[line].is_blank(self.text)
            && setext_level(self.lines[line + 1], self.text).is_some()
        {
            return self.emit_setext_heading(line);
        }
        self.emit_paragraph(line)
    }

    fn find_fence_close(&self, from: usize, open: FenceOpen, prefix: FencePrefix) -> Option<usize> {
        for line_index in from..self.lines.len() {
            let line = self.lines[line_index];
            if line.is_blank(self.text) && self.line_after_prefix(line, prefix).is_none() {
                // Two unindented blank lines end Julia's list container.
                if matches!(prefix, FencePrefix::Spaces(_))
                    && self.lines.get(line_index + 1).is_some_and(|next| {
                        next.is_blank(self.text) && self.line_after_prefix(*next, prefix).is_none()
                    })
                {
                    return None;
                }
                continue;
            }
            let line = self.line_after_prefix(line, prefix)?;
            let content = line.content(self.text);
            let end = content
                .bytes()
                .take_while(|&byte| byte == open.marker)
                .count();
            if end == open.length && content[end..].trim_matches([' ', '\t']).is_empty() {
                return Some(line_index);
            }
        }
        None
    }

    fn line_after_prefix(&self, line: Line, prefix: FencePrefix) -> Option<Line> {
        let start = match prefix {
            FencePrefix::Spaces(prefix) => {
                if !line
                    .content(self.text)
                    .as_bytes()
                    .get(..prefix)?
                    .iter()
                    .all(|&byte| byte == b' ')
                {
                    return None;
                }
                line.start + prefix
            }
            FencePrefix::IndentedContainer => indented_content_start(line, self.text)?,
        };
        Some(Line { start, ..line })
    }

    fn emit_fence(
        &mut self,
        open_line: usize,
        close_line: usize,
        open: FenceOpen,
        prefix: FencePrefix,
        opening_start: usize,
    ) -> usize {
        let opening = self.lines[open_line];
        let closing = self.lines[close_line];
        let info = self.text[open.marker_end..opening.content_end].trim();
        let node_kind = if info == "math" {
            SyntaxKind::MATH_BLOCK
        } else {
            SyntaxKind::FENCED_CODE_BLOCK
        };
        self.start(node_kind);
        self.token(SyntaxKind::WHITESPACE, opening_start, open.marker_start);
        self.token(SyntaxKind::FENCE, open.marker_start, open.marker_end);
        self.token(
            SyntaxKind::INFO_STRING,
            open.marker_end,
            opening.content_end,
        );
        self.token(SyntaxKind::NEWLINE, opening.content_end, opening.end);

        let content_kind = if node_kind == SyntaxKind::MATH_BLOCK {
            SyntaxKind::MATH_CONTENT
        } else {
            SyntaxKind::CODE_CONTENT
        };
        if matches!(prefix, FencePrefix::Spaces(0)) {
            self.token(content_kind, opening.end, closing.start);
        } else {
            for line_index in open_line + 1..close_line {
                let line = self.lines[line_index];
                let content_start = match prefix {
                    FencePrefix::Spaces(prefix) => {
                        line.start + prefix.min(indentation(line.content(self.text)))
                    }
                    FencePrefix::IndentedContainer => {
                        indented_content_start(line, self.text).unwrap_or(line.content_end)
                    }
                };
                self.token(SyntaxKind::WHITESPACE, line.start, content_start);
                self.token(content_kind, content_start, line.end);
            }
        }

        let marker_start = self
            .line_after_prefix(closing, prefix)
            .expect("closing fence retains its container prefix")
            .start;
        let mut marker_end = marker_start;
        while marker_end < closing.content_end && self.text.as_bytes()[marker_end] == open.marker {
            marker_end += 1;
        }
        self.token(SyntaxKind::WHITESPACE, closing.start, marker_start);
        self.token(SyntaxKind::FENCE, marker_start, marker_end);
        self.token(SyntaxKind::MARKER, marker_end, closing.content_end);
        self.token(SyntaxKind::NEWLINE, closing.content_end, closing.end);
        self.finish();
        close_line + 1
    }

    fn emit_atx_heading(&mut self, line_index: usize) -> usize {
        let line = self.lines[line_index];
        let (marker_start, marker_end, content_start, content_end) =
            atx_heading(line, self.text).expect("checked heading");
        self.start(SyntaxKind::ATX_HEADING);
        self.token(SyntaxKind::WHITESPACE, line.start, marker_start);
        self.token(SyntaxKind::HEADING_MARKER, marker_start, marker_end);
        self.token(SyntaxKind::WHITESPACE, marker_end, content_start);
        self.parse_inlines(content_start, content_end);
        self.token(SyntaxKind::MARKER, content_end, line.content_end);
        self.token(SyntaxKind::NEWLINE, line.content_end, line.end);
        self.finish();
        line_index + 1
    }

    fn emit_setext_heading(&mut self, line_index: usize) -> usize {
        let content = self.lines[line_index];
        let underline = self.lines[line_index + 1];
        self.start(SyntaxKind::SETEXT_HEADING);
        self.emit_trimmed_inlines(content);
        self.token(SyntaxKind::NEWLINE, content.content_end, content.end);
        let indent = indentation(underline.content(self.text));
        self.token(
            SyntaxKind::WHITESPACE,
            underline.start,
            underline.start + indent,
        );
        self.token(
            SyntaxKind::HEADING_MARKER,
            underline.start + indent,
            underline.content_end,
        );
        self.token(SyntaxKind::NEWLINE, underline.content_end, underline.end);
        self.finish();
        line_index + 2
    }

    fn emit_table(&mut self, line_index: usize) -> usize {
        self.start(SyntaxKind::TABLE);
        let mut line = line_index;
        while line < self.lines.len()
            && !self.lines[line].is_blank(self.text)
            && contains_unescaped_pipe(self.lines[line].content(self.text))
        {
            self.emit_table_row(self.lines[line]);
            line += 1;
        }
        self.finish();
        line
    }

    fn emit_table_row(&mut self, line: Line) {
        self.start(SyntaxKind::TABLE_ROW);
        let mut cursor = line.start;
        let mut cell_start = line.start;
        while cursor < line.content_end {
            if self.text.as_bytes()[cursor] == b'|'
                && (cursor == line.start || self.text.as_bytes()[cursor - 1] != b'\\')
            {
                if cell_start < cursor {
                    self.start(SyntaxKind::TABLE_CELL);
                    self.parse_inlines(cell_start, cursor);
                    self.finish();
                }
                self.token(SyntaxKind::MARKER, cursor, cursor + 1);
                cursor += 1;
                cell_start = cursor;
            } else {
                cursor += self.text[cursor..].chars().next().unwrap().len_utf8();
            }
        }
        if cell_start < line.content_end {
            self.start(SyntaxKind::TABLE_CELL);
            self.parse_inlines(cell_start, line.content_end);
            self.finish();
        }
        self.token(SyntaxKind::NEWLINE, line.content_end, line.end);
        self.finish();
    }

    fn emit_block_quote(&mut self, line_index: usize) -> usize {
        self.start(SyntaxKind::BLOCK_QUOTE);
        let mut line_index = line_index;
        while line_index < self.lines.len() {
            let line = self.lines[line_index];
            let Some((_marker_start, content_start)) = block_quote_content(line, self.text) else {
                break;
            };
            if content_start == line.content_end {
                self.token(SyntaxKind::BLANK_LINE, line.start, line.end);
                line_index += 1;
                continue;
            }

            if list_marker_range(self.text, content_start, line.content_end).is_some() {
                self.start(SyntaxKind::LIST);
                while line_index < self.lines.len() {
                    let item_line = self.lines[line_index];
                    let Some((quote_marker, quote_content)) =
                        block_quote_content(item_line, self.text)
                    else {
                        break;
                    };
                    let Some((list_start, list_end, item_content)) =
                        list_marker_range(self.text, quote_content, item_line.content_end)
                    else {
                        break;
                    };
                    self.start(SyntaxKind::LIST_ITEM);
                    self.emit_quote_prefix(item_line, quote_marker, quote_content);
                    self.token(SyntaxKind::WHITESPACE, quote_content, list_start);
                    self.token(SyntaxKind::LIST_MARKER, list_start, list_end);
                    self.token(SyntaxKind::WHITESPACE, list_end, item_content);
                    let mut inlines = vec![InlineLine::new(
                        Line {
                            start: item_content,
                            ..item_line
                        },
                        item_content,
                    )];
                    line_index += 1;
                    while line_index < self.lines.len() {
                        let continuation = self.lines[line_index];
                        let Some((quote_marker, quote_content_start)) =
                            block_quote_content(continuation, self.text)
                        else {
                            break;
                        };
                        let indent =
                            indentation(&self.text[quote_content_start..continuation.content_end]);
                        if quote_content_start + indent == continuation.content_end
                            || indent < item_content - quote_content
                            || list_marker_range(
                                self.text,
                                quote_content_start,
                                continuation.content_end,
                            )
                            .is_some()
                        {
                            break;
                        }
                        inlines.push(InlineLine {
                            line: continuation,
                            content_start: quote_content_start + indent,
                            quote_marker: Some(quote_marker),
                        });
                        line_index += 1;
                    }
                    self.flush_inline_lines(&mut inlines, SyntaxKind::NEWLINE);
                    self.finish();
                }
                self.finish();
                continue;
            }

            self.start(SyntaxKind::PARAGRAPH);
            let mut inlines = Vec::new();
            while line_index < self.lines.len() {
                let paragraph_line = self.lines[line_index];
                let Some((quote_marker, quote_content)) =
                    block_quote_content(paragraph_line, self.text)
                else {
                    break;
                };
                if quote_content == paragraph_line.content_end
                    || list_marker_range(self.text, quote_content, paragraph_line.content_end)
                        .is_some()
                {
                    break;
                }
                inlines.push(InlineLine {
                    line: paragraph_line,
                    content_start: quote_content,
                    quote_marker: Some(quote_marker),
                });
                line_index += 1;
            }
            self.flush_inline_lines(&mut inlines, SyntaxKind::SOFT_BREAK);
            self.finish();
        }
        self.finish();
        line_index
    }

    fn emit_quote_prefix(&mut self, line: Line, marker_start: usize, content_start: usize) {
        self.token(SyntaxKind::WHITESPACE, line.start, marker_start);
        self.token(SyntaxKind::QUOTE_MARKER, marker_start, marker_start + 1);
        self.token(SyntaxKind::WHITESPACE, marker_start + 1, content_start);
    }

    fn emit_admonition(&mut self, line_index: usize) -> usize {
        let header = self.lines[line_index];
        let (marker_start, category, title) =
            admonition_header(header, self.text).expect("checked admonition");
        self.start(SyntaxKind::ADMONITION);
        self.token(SyntaxKind::WHITESPACE, header.start, marker_start);
        self.token(SyntaxKind::MARKER, marker_start, marker_start + 3);
        self.token(SyntaxKind::WHITESPACE, marker_start + 3, category.start);
        self.token(
            SyntaxKind::ADMONITION_CATEGORY,
            category.start,
            category.end,
        );
        if let Some(title) = title {
            self.token(SyntaxKind::WHITESPACE, category.end, title.start);
            self.token(SyntaxKind::ADMONITION_TITLE, title.start, title.end);
            self.token(SyntaxKind::WHITESPACE, title.end, header.content_end);
        } else {
            self.token(SyntaxKind::WHITESPACE, category.end, header.content_end);
        }
        self.token(SyntaxKind::NEWLINE, header.content_end, header.end);
        let next = self.emit_indented_container_blocks(line_index + 1);
        self.finish();
        next
    }

    fn emit_indented_container_blocks(&mut self, mut line_index: usize) -> usize {
        while line_index < self.lines.len() {
            let line = self.lines[line_index];
            if line.is_blank(self.text) {
                self.token(SyntaxKind::BLANK_LINE, line.start, line.end);
                line_index += 1;
                continue;
            }
            let Some(content_start) = indented_content_start(line, self.text) else {
                break;
            };
            if let Some(open) = fence_open(
                Line {
                    start: content_start,
                    ..line
                },
                self.text,
            ) {
                if let Some(close) =
                    self.find_fence_close(line_index + 1, open, FencePrefix::IndentedContainer)
                {
                    line_index = self.emit_fence(
                        line_index,
                        close,
                        open,
                        FencePrefix::IndentedContainer,
                        line.start,
                    );
                    continue;
                }
                self.diagnostics.push(ParseDiagnostic {
                    kind: DiagnosticKind::UnclosedFence,
                    range: range(open.marker_start, open.marker_end),
                });
            }
            if let Some((marker_start, marker_end, _)) =
                list_marker_range(self.text, content_start, line.content_end)
            {
                let ordered = self.text.as_bytes()[marker_start..marker_end]
                    .first()
                    .is_some_and(u8::is_ascii_digit);
                line_index = self.emit_list_level(line_index, marker_start - line.start, ordered);
                continue;
            }

            self.start(SyntaxKind::PARAGRAPH);
            let mut inlines = Vec::new();
            while line_index < self.lines.len() {
                let paragraph_line = self.lines[line_index];
                let Some(content_start) = indented_content_start(paragraph_line, self.text) else {
                    break;
                };
                if list_marker_range(self.text, content_start, paragraph_line.content_end).is_some()
                    || (!inlines.is_empty()
                        && fence_open(
                            Line {
                                start: content_start,
                                ..paragraph_line
                            },
                            self.text,
                        )
                        .is_some())
                {
                    break;
                }
                inlines.push(InlineLine::new(paragraph_line, content_start));
                line_index += 1;
                if line_index >= self.lines.len() || self.lines[line_index].is_blank(self.text) {
                    break;
                }
            }
            self.flush_inline_lines(&mut inlines, SyntaxKind::SOFT_BREAK);
            self.finish();
        }
        line_index
    }

    fn emit_footnote(&mut self, line_index: usize) -> usize {
        let header = self.lines[line_index];
        let (label_start, label_end, content_start) =
            footnote_header(header, self.text).expect("checked footnote");
        self.start(SyntaxKind::FOOTNOTE_DEFINITION);
        self.token(SyntaxKind::WHITESPACE, header.start, label_start);
        self.token(SyntaxKind::FOOTNOTE_LABEL, label_start, label_end);
        self.token(SyntaxKind::WHITESPACE, label_end, content_start);
        self.start(SyntaxKind::PARAGRAPH);
        let mut inlines = vec![InlineLine::new(
            Line {
                start: content_start,
                ..header
            },
            content_start,
        )];

        let mut next = line_index + 1;
        while next < self.lines.len() {
            let line = self.lines[next];
            if line.is_blank(self.text) {
                break;
            }
            let Some(content_start) = indented_content_start(line, self.text) else {
                break;
            };
            if list_marker_range(self.text, content_start, line.content_end).is_some() {
                break;
            }
            inlines.push(InlineLine::new(line, content_start));
            next += 1;
        }
        self.flush_inline_lines(&mut inlines, SyntaxKind::SOFT_BREAK);
        self.finish();
        next = self.emit_indented_container_blocks(next);
        self.finish();
        next
    }

    fn emit_list(&mut self, line_index: usize) -> usize {
        let (marker_start, marker_end, _) =
            list_marker(self.lines[line_index], self.text).expect("checked list");
        let base_indent = marker_start - self.lines[line_index].start;
        let ordered = self.text.as_bytes()[marker_start..marker_end]
            .first()
            .is_some_and(u8::is_ascii_digit);
        self.emit_list_level(line_index, base_indent, ordered)
    }

    fn emit_list_level(
        &mut self,
        mut line_index: usize,
        base_indent: usize,
        ordered: bool,
    ) -> usize {
        self.start(SyntaxKind::LIST);
        while line_index < self.lines.len() {
            let line = self.lines[line_index];
            let Some((marker_start, marker_end, content_start)) = list_marker_any(line, self.text)
            else {
                break;
            };
            let indent = marker_start - line.start;
            let item_ordered = self.text.as_bytes()[marker_start..marker_end]
                .first()
                .is_some_and(u8::is_ascii_digit);
            if indent != base_indent || item_ordered != ordered {
                break;
            }
            self.start(SyntaxKind::LIST_ITEM);
            self.token(SyntaxKind::WHITESPACE, line.start, marker_start);
            self.token(SyntaxKind::LIST_MARKER, marker_start, marker_end);
            self.token(SyntaxKind::WHITESPACE, marker_end, content_start);
            let item_indent = marker_end - line.start + usize::from(content_start > marker_end);
            let mut inlines = Vec::new();
            let first_fence = fence_open(
                Line {
                    start: line.start + item_indent,
                    ..line
                },
                self.text,
            )
            .and_then(|open| {
                if let Some(close) =
                    self.find_fence_close(line_index + 1, open, FencePrefix::Spaces(item_indent))
                {
                    Some((open, close))
                } else {
                    self.diagnostics.push(ParseDiagnostic {
                        kind: DiagnosticKind::UnclosedFence,
                        range: range(open.marker_start, open.marker_end),
                    });
                    None
                }
            });
            if let Some((open, close)) = first_fence {
                line_index = self.emit_fence(
                    line_index,
                    close,
                    open,
                    FencePrefix::Spaces(item_indent),
                    content_start,
                );
            } else {
                inlines.push(InlineLine::new(
                    Line {
                        start: content_start,
                        ..line
                    },
                    content_start,
                ));
                line_index += 1;
            }
            let mut after_blank = false;

            while line_index < self.lines.len() {
                let continuation = self.lines[line_index];
                if continuation.is_blank(self.text) {
                    let Some(next) = self.lines.get(line_index + 1).copied() else {
                        break;
                    };
                    if next.is_blank(self.text) {
                        break;
                    }
                    let continues_list = list_marker_any(next, self.text)
                        .is_some_and(|(start, _, _)| start - next.start >= base_indent);
                    let continues_item = indentation(next.content(self.text)) > base_indent;
                    if !continues_list && !continues_item {
                        break;
                    }
                    self.flush_inline_lines(&mut inlines, SyntaxKind::NEWLINE);
                    self.token(SyntaxKind::BLANK_LINE, continuation.start, continuation.end);
                    line_index += 1;
                    after_blank = true;
                    continue;
                }
                if let Some((nested_start, nested_end, _)) =
                    list_marker_any(continuation, self.text)
                {
                    let nested_indent = nested_start - continuation.start;
                    let nested_ordered = self.text.as_bytes()[nested_start..nested_end]
                        .first()
                        .is_some_and(u8::is_ascii_digit);
                    if nested_indent > base_indent {
                        self.flush_inline_lines(&mut inlines, SyntaxKind::NEWLINE);
                        line_index =
                            self.emit_list_level(line_index, nested_indent, nested_ordered);
                        after_blank = false;
                        continue;
                    }
                    break;
                }
                let indent = indentation(continuation.content(self.text));
                if indent <= base_indent {
                    break;
                }
                if after_blank
                    && indented_content_start_after(continuation, self.text, item_indent).is_some()
                {
                    self.flush_inline_lines(&mut inlines, SyntaxKind::NEWLINE);
                    line_index = self.emit_indented_code_after(line_index, item_indent);
                    after_blank = false;
                    continue;
                }
                if let Some(open) = self
                    .line_after_prefix(continuation, FencePrefix::Spaces(item_indent))
                    .and_then(|line| fence_open(line, self.text))
                {
                    if let Some(close) = self.find_fence_close(
                        line_index + 1,
                        open,
                        FencePrefix::Spaces(item_indent),
                    ) {
                        self.flush_inline_lines(&mut inlines, SyntaxKind::NEWLINE);
                        line_index = self.emit_fence(
                            line_index,
                            close,
                            open,
                            FencePrefix::Spaces(item_indent),
                            continuation.start,
                        );
                        after_blank = false;
                        continue;
                    }
                    self.diagnostics.push(ParseDiagnostic {
                        kind: DiagnosticKind::UnclosedFence,
                        range: range(open.marker_start, open.marker_end),
                    });
                }
                inlines.push(InlineLine::new(continuation, continuation.start + indent));
                line_index += 1;
                after_blank = false;
            }
            self.flush_inline_lines(&mut inlines, SyntaxKind::NEWLINE);
            self.finish();
        }
        self.finish();
        line_index
    }

    fn emit_indented_code(&mut self, line_index: usize) -> usize {
        self.emit_indented_code_after(line_index, 0)
    }

    fn emit_indented_code_after(&mut self, line_index: usize, prefix: usize) -> usize {
        self.start(SyntaxKind::INDENTED_CODE_BLOCK);
        let mut line_index = line_index;
        while line_index < self.lines.len() {
            let line = self.lines[line_index];
            if line.is_blank(self.text) {
                self.token(SyntaxKind::CODE_CONTENT, line.start, line.end);
                line_index += 1;
                continue;
            }
            let Some(content_start) = indented_content_start_after(line, self.text, prefix) else {
                break;
            };
            self.token(SyntaxKind::WHITESPACE, line.start, content_start);
            self.token(SyntaxKind::CODE_CONTENT, content_start, line.content_end);
            self.token(SyntaxKind::NEWLINE, line.content_end, line.end);
            line_index += 1;
        }
        self.finish();
        line_index
    }

    fn emit_math_block(&mut self, line_index: usize) -> usize {
        let line = self.lines[line_index];
        let (open, content_start, content_end, close) =
            math_block(line, self.text).expect("checked math block");
        self.start(SyntaxKind::MATH_BLOCK);
        self.token(SyntaxKind::WHITESPACE, line.start, open);
        self.token(SyntaxKind::MARKER, open, content_start);
        self.token(SyntaxKind::MATH_CONTENT, content_start, content_end);
        self.token(SyntaxKind::MARKER, content_end, close);
        self.token(SyntaxKind::WHITESPACE, close, line.content_end);
        self.token(SyntaxKind::NEWLINE, line.content_end, line.end);
        self.finish();
        line_index + 1
    }

    fn emit_block_interpolation(&mut self, line_index: usize, expression_end: usize) -> usize {
        let line = self.lines[line_index];
        self.start(SyntaxKind::INTERPOLATION);
        self.token(SyntaxKind::MARKER, line.start, line.start + 1);
        self.token(SyntaxKind::TEXT, line.start + 1, expression_end);
        self.token(SyntaxKind::WHITESPACE, expression_end, line.content_end);
        self.token(SyntaxKind::NEWLINE, line.content_end, line.end);
        self.finish();
        line_index + 1
    }

    fn emit_simple_line(&mut self, line_index: usize, kind: SyntaxKind) -> usize {
        let line = self.lines[line_index];
        self.start(kind);
        self.token(SyntaxKind::MARKER, line.start, line.content_end);
        self.token(SyntaxKind::NEWLINE, line.content_end, line.end);
        self.finish();
        line_index + 1
    }

    fn emit_paragraph(&mut self, line_index: usize) -> usize {
        self.start(SyntaxKind::PARAGRAPH);
        let mut current = line_index;
        let mut inlines = Vec::new();
        while current < self.lines.len() && !self.lines[current].is_blank(self.text) {
            if current > line_index && self.is_breaking_block(current) {
                break;
            }
            let line = self.lines[current];
            inlines.push(InlineLine::new(line, line.start));
            current += 1;
        }
        self.flush_inline_lines(&mut inlines, SyntaxKind::SOFT_BREAK);
        self.finish();
        current
    }

    fn is_breaking_block(&self, line: usize) -> bool {
        let current = self.lines[line];
        fence_open(current, self.text).is_some()
            || atx_heading(current, self.text).is_some()
            || block_quote_content(current, self.text).is_some()
            || admonition_header(current, self.text).is_some()
            || footnote_header(current, self.text).is_some()
            || list_marker(current, self.text).is_some()
    }

    fn emit_trimmed_inlines(&mut self, line: Line) {
        let raw = line.content(self.text);
        let leading = raw.len() - raw.trim_start().len();
        let trailing = raw.trim_end().len();
        self.token(SyntaxKind::WHITESPACE, line.start, line.start + leading);
        self.parse_inlines(line.start + leading, line.start + trailing);
        self.token(
            SyntaxKind::WHITESPACE,
            line.start + trailing,
            line.content_end,
        );
    }

    fn parse_inlines(&mut self, start: usize, end: usize) {
        InlineParser {
            text: self.text,
            source: self.text,
            builder: &mut self.builder,
            pieces: &[],
            piece_index: 0,
            literal_end: end,
        }
        .parse_inlines(start, end);
    }

    fn flush_inline_lines(&mut self, lines: &mut Vec<InlineLine>, final_break: SyntaxKind) {
        let Some(last) = lines.last().copied() else {
            return;
        };
        let last_hard_break = last.content_start < last.line.content_end
            && self.text.as_bytes()[last.line.content_end - 1] == b'\\'
            && last.line.content_end < last.line.end;
        if lines.len() == 1 && !last_hard_break {
            if let Some(marker) = last.quote_marker {
                self.emit_quote_prefix(last.line, marker, last.content_start);
            } else {
                self.token(SyntaxKind::WHITESPACE, last.line.start, last.content_start);
            }
            self.parse_inlines(last.content_start, last.line.content_end);
            self.token(final_break, last.line.content_end, last.line.end);
            lines.clear();
            return;
        }
        let mut text = String::new();
        let mut pieces = Vec::new();
        for (index, inline) in lines.iter().enumerate() {
            let line = inline.line;
            let mut prefix = |kind, start, end| {
                if start < end {
                    pieces.push(InlinePiece {
                        logical: text.len()..text.len(),
                        source: start..end,
                        kind: InlinePieceKind::Prefix(kind),
                    });
                }
            };
            if let Some(marker) = inline.quote_marker {
                prefix(SyntaxKind::WHITESPACE, line.start, marker);
                prefix(SyntaxKind::QUOTE_MARKER, marker, marker + 1);
                prefix(SyntaxKind::WHITESPACE, marker + 1, inline.content_start);
            } else {
                prefix(SyntaxKind::WHITESPACE, line.start, inline.content_start);
            }
            if inline.content_start < line.content_end {
                let start = text.len();
                text.push_str(&self.text[inline.content_start..line.content_end]);
                pieces.push(InlinePiece {
                    logical: start..text.len(),
                    source: inline.content_start..line.content_end,
                    kind: InlinePieceKind::Content,
                });
            }
            let hard_break = inline.content_start < line.content_end
                && self.text.as_bytes()[line.content_end - 1] == b'\\'
                && line.content_end < line.end;
            if index + 1 < lines.len() || hard_break {
                let start = text.len();
                text.push(if hard_break { '\n' } else { ' ' });
                pieces.push(InlinePiece {
                    logical: start..text.len(),
                    source: line.content_end..line.end,
                    kind: if hard_break {
                        InlinePieceKind::HardBreak
                    } else {
                        InlinePieceKind::SoftBreak
                    },
                });
            }
        }
        let mut parser = InlineParser {
            text: &text,
            source: self.text,
            builder: &mut self.builder,
            pieces: &pieces,
            piece_index: 0,
            literal_end: text.find('\n').unwrap_or(text.len()),
        };
        parser.parse_inlines(0, text.len());
        // An empty final line can leave only its container prefix unconsumed.
        for piece in &pieces[parser.piece_index..] {
            if let InlinePieceKind::Prefix(kind) = piece.kind {
                self.token(kind, piece.source.start, piece.source.end);
            }
        }
        if !last_hard_break {
            self.token(final_break, last.line.content_end, last.line.end);
        }
        lines.clear();
    }

    fn start(&mut self, kind: SyntaxKind) {
        self.builder
            .start_node(DocumentationLanguage::kind_to_raw(kind));
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    fn token(&mut self, kind: SyntaxKind, start: usize, end: usize) {
        if start < end {
            self.builder.token(
                DocumentationLanguage::kind_to_raw(kind),
                &self.text[start..end],
            );
        }
    }
}

// Inline recognition uses paragraph text, while token emission retains the
// original bytes and container prefixes at their decoded-text offsets.
struct InlineParser<'a, 'b> {
    text: &'a str,
    source: &'a str,
    builder: &'b mut GreenNodeBuilder<'static>,
    pieces: &'a [InlinePiece],
    piece_index: usize,
    literal_end: usize,
}

impl InlineParser<'_, '_> {
    fn parse_inlines(&mut self, start: usize, end: usize) {
        let mut cursor = start;
        let mut text_start = start;
        while cursor < end {
            let byte = self.text.as_bytes()[cursor];
            let trigger = matches!(byte, b'\\' | b'!' | b'[' | b'<' | b'*' | b'_' | b'`' | b'$')
                || (byte == b'-' && cursor + 1 < end && self.text.as_bytes()[cursor + 1] == b'-');
            if !trigger {
                cursor += self.text[cursor..end].chars().next().unwrap().len_utf8();
                continue;
            }

            // Inline helpers emit their nodes immediately. Flush the text that
            // precedes a possible opener first so Rowan sees source order.
            self.token(SyntaxKind::TEXT, text_start, cursor);
            let opener = cursor;
            let parsed = match self.text.as_bytes()[cursor] {
                b'\\' => self.emit_escape(cursor, end),
                b'!' if cursor + 1 < end && self.text.as_bytes()[cursor + 1] == b'[' => {
                    self.emit_image(cursor, end)
                }
                b'[' => self.emit_link_or_footnote(cursor, end),
                b'<' => self.emit_autolink(cursor, end),
                b'*' | b'_' => self.emit_emphasis(cursor, end),
                b'`' => self.emit_backticks(cursor, end),
                b'$' => self.emit_dollar(cursor, end),
                b'-' if cursor + 1 < end && self.text.as_bytes()[cursor + 1] == b'-' => {
                    if cursor + 2 < end && self.text.as_bytes()[cursor + 2] == b'-' {
                        Some((SyntaxKind::EM_DASH, cursor + 3))
                    } else {
                        Some((SyntaxKind::EN_DASH, cursor + 2))
                    }
                }
                _ => None,
            };
            let Some((kind, next)) = parsed else {
                cursor += self.text[cursor..end].chars().next().unwrap().len_utf8();
                text_start = opener;
                continue;
            };
            if matches!(
                kind,
                SyntaxKind::ESCAPE | SyntaxKind::EN_DASH | SyntaxKind::EM_DASH
            ) {
                self.token(kind, cursor, next);
            }
            cursor = next;
            text_start = cursor;
        }
        self.token(SyntaxKind::TEXT, text_start, end);
    }

    fn emit_escape(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        let next = start + 1;
        if next >= end {
            return None;
        }
        let ch = self.text[next..end].chars().next()?;
        if ch == '\n' {
            self.start(SyntaxKind::HARD_BREAK);
            self.token(SyntaxKind::ESCAPE, start, next);
            self.token(SyntaxKind::NEWLINE, next, next + 1);
            self.finish();
            return Some((SyntaxKind::HARD_BREAK, next + 1));
        }
        "\\`*_#+-.!{}[]()$"
            .contains(ch)
            .then_some((SyntaxKind::ESCAPE, next + ch.len_utf8()))
    }

    fn emit_image(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        let end = self.literal_inline_end(start, end);
        let label_end = find_closing(self.text, start + 2, end, b'[', b']')?;
        let mut open = label_end + 1;
        while open < end && self.text.as_bytes()[open].is_ascii_whitespace() {
            open += 1;
        }
        if open >= end || self.text.as_bytes()[open] != b'(' {
            return None;
        }
        let destination_end = find_closing(self.text, open + 1, end, b'(', b')')?;
        self.start(SyntaxKind::IMAGE);
        self.token(SyntaxKind::MARKER, start, start + 2);
        self.literal_token(SyntaxKind::TEXT, start + 2, label_end);
        self.token(SyntaxKind::MARKER, label_end, open + 1);
        self.token(SyntaxKind::LINK_DESTINATION, open + 1, destination_end);
        self.token(SyntaxKind::MARKER, destination_end, destination_end + 1);
        self.finish();
        Some((SyntaxKind::IMAGE, destination_end + 1))
    }

    fn emit_link_or_footnote(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        let end = self.literal_inline_end(start, end);
        if start + 2 < end && self.text.as_bytes()[start + 1] == b'^' {
            let close = self.text[start + 2..end].find(']')? + start + 2;
            if self.text[start + 2..close]
                .chars()
                .all(|ch| ch.is_alphanumeric() || ch == '_')
            {
                self.start(SyntaxKind::FOOTNOTE_REFERENCE);
                self.token(SyntaxKind::MARKER, start, start + 2);
                self.token(SyntaxKind::FOOTNOTE_LABEL, start + 2, close);
                self.token(SyntaxKind::MARKER, close, close + 1);
                self.finish();
                return Some((SyntaxKind::FOOTNOTE_REFERENCE, close + 1));
            }
        }

        let label_end = find_closing(self.text, start + 1, end, b'[', b']')?;
        let open = label_end + 1;
        if open >= end || self.text.as_bytes()[open] != b'(' {
            return None;
        }
        let destination_end = find_closing(self.text, open + 1, end, b'(', b')')?;
        self.start(SyntaxKind::LINK);
        self.token(SyntaxKind::MARKER, start, start + 1);
        self.parse_inlines(start + 1, label_end);
        self.token(SyntaxKind::MARKER, label_end, open + 1);
        self.token(SyntaxKind::LINK_DESTINATION, open + 1, destination_end);
        self.token(SyntaxKind::MARKER, destination_end, destination_end + 1);
        self.finish();
        Some((SyntaxKind::LINK, destination_end + 1))
    }

    fn emit_autolink(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        let end = self.literal_inline_end(start, end);
        let close = self.text[start + 1..end].find('>')? + start + 1;
        let destination = &self.text[start + 1..close];
        if !is_autolink(destination) {
            return None;
        }
        self.start(SyntaxKind::AUTOLINK);
        self.token(SyntaxKind::MARKER, start, start + 1);
        self.token(SyntaxKind::LINK_DESTINATION, start + 1, close);
        self.token(SyntaxKind::MARKER, close, close + 1);
        self.finish();
        Some((SyntaxKind::AUTOLINK, close + 1))
    }

    fn emit_emphasis(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        let byte = self.text.as_bytes()[start];
        let length = if start + 1 < end && self.text.as_bytes()[start + 1] == byte {
            2
        } else {
            1
        };
        let delimiter = &self.text[start..start + length];
        let close = self.text[start + length..end].find(delimiter)? + start + length;
        if close == start + length {
            return None;
        }
        self.start(if length == 2 {
            SyntaxKind::STRONG
        } else {
            SyntaxKind::EMPHASIS
        });
        self.token(SyntaxKind::MARKER, start, start + length);
        self.parse_inlines(start + length, close);
        self.token(SyntaxKind::MARKER, close, close + length);
        self.finish();
        Some((SyntaxKind::EMPHASIS, close + length))
    }

    fn emit_backticks(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        let end = self.literal_inline_end(start, end);
        let mut open_end = start;
        while open_end < end && self.text.as_bytes()[open_end] == b'`' {
            open_end += 1;
        }
        let delimiter = &self.text[start..open_end];
        let close = self.text[open_end..end].find(delimiter)? + open_end;
        let kind = if delimiter.len().is_multiple_of(2) {
            SyntaxKind::INLINE_MATH
        } else {
            SyntaxKind::INLINE_CODE
        };
        self.start(kind);
        self.token(SyntaxKind::MARKER, start, open_end);
        self.token(
            if kind == SyntaxKind::INLINE_MATH {
                SyntaxKind::MATH_CONTENT
            } else {
                SyntaxKind::CODE_CONTENT
            },
            open_end,
            close,
        );
        self.token(SyntaxKind::MARKER, close, close + delimiter.len());
        self.finish();
        Some((kind, close + delimiter.len()))
    }

    fn emit_dollar(&mut self, start: usize, end: usize) -> Option<(SyntaxKind, usize)> {
        if let Some(relative) = self.text[start + 1..end].find('$') {
            let close = start + 1 + relative;
            self.start(SyntaxKind::INLINE_MATH);
            self.token(SyntaxKind::MARKER, start, start + 1);
            self.token(SyntaxKind::MATH_CONTENT, start + 1, close);
            self.token(SyntaxKind::MARKER, close, close + 1);
            self.finish();
            return Some((SyntaxKind::INLINE_MATH, close + 1));
        }
        let expression_end = interpolation_end(self.text, start, end)?;
        self.start(SyntaxKind::INTERPOLATION);
        self.token(SyntaxKind::MARKER, start, start + 1);
        self.literal_token(SyntaxKind::TEXT, start + 1, expression_end);
        self.finish();
        Some((SyntaxKind::INTERPOLATION, expression_end))
    }

    fn literal_inline_end(&mut self, start: usize, end: usize) -> usize {
        // Julia's literal inline readers stop at hard breaks. Ordinary wraps
        // have already become spaces, so they do not truncate these readers.
        if start > self.literal_end {
            self.literal_end = self.text[start..]
                .find('\n')
                .map_or(self.text.len(), |offset| start + offset);
        }
        end.min(self.literal_end)
    }

    fn start(&mut self, kind: SyntaxKind) {
        // A container prefix precedes an opener on the next line, so it
        // belongs outside that new inline node's source range.
        while let Some(piece) = self.pieces.get(self.piece_index)
            && let InlinePieceKind::Prefix(kind) = piece.kind
        {
            self.builder.token(
                DocumentationLanguage::kind_to_raw(kind),
                &self.source[piece.source.clone()],
            );
            self.piece_index += 1;
        }
        self.builder
            .start_node(DocumentationLanguage::kind_to_raw(kind));
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    fn token(&mut self, kind: SyntaxKind, start: usize, end: usize) {
        let soft_break = if kind == SyntaxKind::TEXT {
            SyntaxKind::SOFT_BREAK
        } else {
            kind
        };
        self.mapped_token(kind, start, end, soft_break);
    }

    fn literal_token(&mut self, kind: SyntaxKind, start: usize, end: usize) {
        self.mapped_token(kind, start, end, kind);
    }

    fn mapped_token(&mut self, kind: SyntaxKind, start: usize, end: usize, soft_break: SyntaxKind) {
        if start == end {
            return;
        }
        if self.pieces.is_empty() {
            self.builder.token(
                DocumentationLanguage::kind_to_raw(kind),
                &self.source[start..end],
            );
            return;
        }
        let mut cursor = start;
        while cursor < end {
            let piece = &self.pieces[self.piece_index];
            let (source_start, source_end, token_kind, next) = match piece.kind {
                InlinePieceKind::Content => {
                    let next = end.min(piece.logical.end);
                    (
                        piece.source.start + cursor - piece.logical.start,
                        piece.source.start + next - piece.logical.start,
                        kind,
                        next,
                    )
                }
                InlinePieceKind::SoftBreak => {
                    (piece.source.start, piece.source.end, soft_break, cursor + 1)
                }
                InlinePieceKind::HardBreak => (
                    piece.source.start,
                    piece.source.end,
                    SyntaxKind::NEWLINE,
                    cursor + 1,
                ),
                InlinePieceKind::Prefix(prefix_kind) => {
                    (piece.source.start, piece.source.end, prefix_kind, cursor)
                }
            };
            self.builder.token(
                DocumentationLanguage::kind_to_raw(token_kind),
                &self.source[source_start..source_end],
            );
            cursor = next;
            if cursor == piece.logical.end {
                self.piece_index += 1;
            }
        }
    }
}

fn lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let Some(relative) = text[start..].find('\n') else {
            out.push(Line {
                start,
                content_end: text.len(),
                end: text.len(),
            });
            break;
        };
        let newline = start + relative;
        let content_end = if newline > start && text.as_bytes()[newline - 1] == b'\r' {
            newline - 1
        } else {
            newline
        };
        out.push(Line {
            start,
            content_end,
            end: newline + 1,
        });
        start = newline + 1;
    }
    out
}

fn indentation(text: &str) -> usize {
    text.as_bytes()
        .iter()
        .take_while(|&&byte| byte == b' ')
        .count()
}

fn indented_content_start(line: Line, text: &str) -> Option<usize> {
    indented_content_start_after(line, text, 0)
}

fn indented_content_start_after(line: Line, text: &str, prefix: usize) -> Option<usize> {
    let content = line.content(text);
    if content.len() < prefix
        || !content.as_bytes()[..prefix]
            .iter()
            .all(|&byte| byte == b' ')
    {
        return None;
    }
    let tail = &content[prefix..];
    if tail.starts_with('\t') {
        Some(line.start + prefix + 1)
    } else {
        (indentation(tail) >= 4).then_some(line.start + prefix + 4)
    }
}

fn fence_open(line: Line, text: &str) -> Option<FenceOpen> {
    let content = line.content(text);
    let indent = indentation(content);
    if indent > 3 {
        return None;
    }
    let marker = *content.as_bytes().get(indent)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let mut end = indent;
    while end < content.len() && content.as_bytes()[end] == marker {
        end += 1;
    }
    if end - indent < 3 || content.as_bytes()[end..].contains(&marker) {
        return None;
    }
    Some(FenceOpen {
        marker_start: line.start + indent,
        marker_end: line.start + end,
        marker,
        length: end - indent,
    })
}

fn atx_heading(line: Line, text: &str) -> Option<(usize, usize, usize, usize)> {
    let content = line.content(text);
    let indent = indentation(content);
    if indent > 3 {
        return None;
    }
    let bytes = content.as_bytes();
    let mut marker_end = indent;
    while marker_end < bytes.len() && bytes[marker_end] == b'#' {
        marker_end += 1;
    }
    let level = marker_end - indent;
    if !(1..=6).contains(&level)
        || (marker_end < bytes.len() && !bytes[marker_end].is_ascii_whitespace())
    {
        return None;
    }
    let mut content_start = marker_end;
    while content_start < bytes.len() && matches!(bytes[content_start], b' ' | b'\t') {
        content_start += 1;
    }
    let mut content_end = content.len();
    while content_end > content_start && matches!(bytes[content_end - 1], b' ' | b'\t') {
        content_end -= 1;
    }
    let trailing_end = content_end;
    while content_end > content_start && bytes[content_end - 1] == b'#' {
        content_end -= 1;
    }
    if content_end < trailing_end
        && content_end > content_start
        && bytes[content_end - 1].is_ascii_whitespace()
    {
        while content_end > content_start && bytes[content_end - 1].is_ascii_whitespace() {
            content_end -= 1;
        }
    } else {
        content_end = trailing_end;
    }
    Some((
        line.start + indent,
        line.start + marker_end,
        line.start + content_start,
        line.start + content_end,
    ))
}

fn setext_level(line: Line, text: &str) -> Option<u8> {
    let trimmed = line.content(text).trim_matches([' ', '\t']);
    if trimmed.len() < 3 {
        return None;
    }
    let marker = trimmed.as_bytes()[0];
    if !matches!(marker, b'=' | b'-') || !trimmed.bytes().all(|byte| byte == marker) {
        return None;
    }
    Some(if marker == b'=' { 1 } else { 2 })
}

fn block_quote_content(line: Line, text: &str) -> Option<(usize, usize)> {
    let content = line.content(text);
    let indent = indentation(content);
    if indent > 3 || content.as_bytes().get(indent) != Some(&b'>') {
        return None;
    }
    let marker = line.start + indent;
    let content = if text.as_bytes().get(marker + 1) == Some(&b' ') {
        marker + 2
    } else {
        marker + 1
    };
    Some((marker, content))
}

fn admonition_header(
    line: Line,
    text: &str,
) -> Option<(
    usize,
    std::ops::Range<usize>,
    Option<std::ops::Range<usize>>,
)> {
    let content = line.content(text);
    let indent = indentation(content);
    if indent > 3 || !content[indent..].starts_with("!!! ") {
        return None;
    }
    let marker_start = line.start + indent;
    let rest_start = marker_start + 4;
    let rest = &text[rest_start..line.content_end];
    let category_len = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_lowercase())
        .count();
    if category_len == 0 {
        return None;
    }
    let category = rest_start..rest_start + category_len;
    let tail = text[category.end..line.content_end].trim();
    let title = if tail.is_empty() {
        None
    } else if tail.starts_with('"') && tail.ends_with('"') && tail.len() >= 2 {
        // End at the closing quote: trailing spaces inside the token would
        // survive `Admonition::title`'s quote trimming.
        let offset = text[category.end..line.content_end].find('"')?;
        Some(category.end + offset..category.end + offset + tail.len())
    } else {
        return None;
    };
    Some((marker_start, category, title))
}

fn footnote_header(line: Line, text: &str) -> Option<(usize, usize, usize)> {
    let content = line.content(text);
    let indent = indentation(content);
    if indent > 3 || !content[indent..].starts_with("[^") {
        return None;
    }
    let label_start = line.start + indent;
    let close = text[label_start + 2..line.content_end].find("]:")? + label_start + 2;
    if !text[label_start + 2..close]
        .chars()
        .all(|ch| ch.is_alphanumeric() || ch == '_')
    {
        return None;
    }
    let label_end = close + 2;
    let mut content_start = label_end;
    while content_start < line.content_end && text.as_bytes()[content_start].is_ascii_whitespace() {
        content_start += 1;
    }
    Some((label_start, label_end, content_start))
}

fn list_marker(line: Line, text: &str) -> Option<(usize, usize, usize)> {
    let marker = list_marker_any(line, text)?;
    (marker.0 - line.start <= 3).then_some(marker)
}

fn list_marker_any(line: Line, text: &str) -> Option<(usize, usize, usize)> {
    list_marker_range(text, line.start, line.content_end)
}

fn list_marker_range(text: &str, start: usize, end_offset: usize) -> Option<(usize, usize, usize)> {
    let content = &text[start..end_offset];
    let indent = indentation(content);
    let bytes = content.as_bytes();
    let mut end = indent;
    if matches!(bytes.get(end), Some(b'*' | b'+' | b'-')) {
        end += 1;
    } else {
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == indent || !matches!(bytes.get(end), Some(b'.' | b')')) {
            return None;
        }
        end += 1;
    }
    if end < bytes.len() && !bytes[end].is_ascii_whitespace() {
        return None;
    }
    let mut content_start = end;
    while content_start < bytes.len() && bytes[content_start].is_ascii_whitespace() {
        content_start += 1;
    }
    Some((start + indent, start + end, start + content_start))
}

fn thematic_break(content: &str) -> bool {
    let mut marker = None;
    let mut count = 0;
    for byte in content.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if !matches!(byte, b'*' | b'-') {
            return false;
        }
        if marker.is_some_and(|current| current != byte) {
            return false;
        }
        marker = Some(byte);
        count += 1;
    }
    count >= 3
}

fn math_block(line: Line, text: &str) -> Option<(usize, usize, usize, usize)> {
    let raw = line.content(text);
    let trimmed = raw.trim();
    if trimmed.len() < 2 || !trimmed.starts_with('$') || !trimmed.ends_with('$') {
        return None;
    }
    let open = line.start + raw.len() - raw.trim_start().len();
    let close = open + trimmed.len();
    Some((open, open + 1, close - 1, close))
}

fn block_interpolation(line: Line, text: &str) -> Option<usize> {
    if text.as_bytes().get(line.start) != Some(&b'$') {
        return None;
    }
    let end = interpolation_end(text, line.start, line.content_end)?;
    text[end..line.content_end]
        .trim_matches([' ', '\t'])
        .is_empty()
        .then_some(end)
}

fn contains_unescaped_pipe(text: &str) -> bool {
    text.as_bytes()
        .iter()
        .enumerate()
        .any(|(index, &byte)| byte == b'|' && (index == 0 || text.as_bytes()[index - 1] != b'\\'))
}

fn table_alignment(text: &str) -> bool {
    let trimmed = text.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    let mut cells = 0;
    for cell in trimmed.split('|') {
        let cell = cell.trim();
        if cell.len() < 3 || !cell.bytes().all(|byte| matches!(byte, b'-' | b':')) {
            return false;
        }
        cells += 1;
    }
    cells > 0
}

fn find_closing(text: &str, start: usize, end: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut cursor = start;
    while cursor < end {
        let byte = text.as_bytes()[cursor];
        if byte == b'\\' {
            cursor += 1;
            if cursor < end {
                cursor += text[cursor..end].chars().next()?.len_utf8();
            }
            continue;
        }
        if byte == open {
            depth += 1;
        } else if byte == close {
            if depth == 0 {
                return Some(cursor);
            }
            depth -= 1;
        }
        cursor += text[cursor..end].chars().next()?.len_utf8();
    }
    None
}

fn interpolation_end(text: &str, start: usize, end: usize) -> Option<usize> {
    let first = start + 1;
    if first >= end || text.as_bytes()[first].is_ascii_whitespace() {
        return None;
    }
    if text.as_bytes()[first] == b'(' {
        return find_closing(text, first + 1, end, b'(', b')').map(|close| close + 1);
    }
    let mut cursor = first;
    let mut saw_name = false;
    while cursor < end {
        let ch = text[cursor..end].chars().next()?;
        if ch.is_alphanumeric() || ch == '_' || ch == '!' || ch == '?' || !ch.is_ascii() {
            saw_name = true;
            cursor += ch.len_utf8();
        } else if ch == '.' {
            let after_dot = cursor + 1;
            let Some(next) = text[after_dot..end].chars().next() else {
                break;
            };
            if next.is_alphanumeric() || next == '_' || !next.is_ascii() {
                cursor = after_dot;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    if saw_name && cursor < end && text.as_bytes()[cursor] == b'(' {
        cursor = find_closing(text, cursor + 1, end, b'(', b')')? + 1;
    }
    saw_name.then_some(cursor)
}

fn is_autolink(text: &str) -> bool {
    text.contains("://")
        || text.starts_with("mailto:")
        || (text.contains('@') && !text.chars().any(char::is_whitespace))
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32))
}
