//! Typed navigation over documentation CST nodes.

use rowan::ast::AstNode;
use rowan::{TextRange, TextSize};

use super::syntax::{DocumentationLanguage, SyntaxKind, SyntaxNode, SyntaxToken};

macro_rules! ast_node {
    ($name:ident, $($kind:path)|+) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            type Language = DocumentationLanguage;

            fn can_cast(kind: SyntaxKind) -> bool {
                matches!(kind, $($kind)|+)
            }

            fn cast(syntax: SyntaxNode) -> Option<Self> {
                Self::can_cast(syntax.kind()).then_some(Self(syntax))
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(Document, SyntaxKind::ROOT);
ast_node!(Paragraph, SyntaxKind::PARAGRAPH);
ast_node!(
    Heading,
    SyntaxKind::ATX_HEADING | SyntaxKind::SETEXT_HEADING
);
ast_node!(BlockQuote, SyntaxKind::BLOCK_QUOTE);
ast_node!(Admonition, SyntaxKind::ADMONITION);
ast_node!(List, SyntaxKind::LIST);
ast_node!(ListItem, SyntaxKind::LIST_ITEM);
ast_node!(IndentedCodeBlock, SyntaxKind::INDENTED_CODE_BLOCK);
ast_node!(CodeBlock, SyntaxKind::FENCED_CODE_BLOCK);
ast_node!(MathBlock, SyntaxKind::MATH_BLOCK);
ast_node!(FootnoteDefinition, SyntaxKind::FOOTNOTE_DEFINITION);
ast_node!(Table, SyntaxKind::TABLE);
ast_node!(TableRow, SyntaxKind::TABLE_ROW);
ast_node!(TableCell, SyntaxKind::TABLE_CELL);
ast_node!(ThematicBreak, SyntaxKind::THEMATIC_BREAK);
ast_node!(Emphasis, SyntaxKind::EMPHASIS);
ast_node!(Strong, SyntaxKind::STRONG);
ast_node!(InlineCode, SyntaxKind::INLINE_CODE);
ast_node!(InlineMath, SyntaxKind::INLINE_MATH);
ast_node!(Link, SyntaxKind::LINK);
ast_node!(Image, SyntaxKind::IMAGE);
ast_node!(Autolink, SyntaxKind::AUTOLINK);
ast_node!(FootnoteReference, SyntaxKind::FOOTNOTE_REFERENCE);
ast_node!(Interpolation, SyntaxKind::INTERPOLATION);
ast_node!(HardBreak, SyntaxKind::HARD_BREAK);

/// A top-level documentation block.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Block {
    Paragraph(Paragraph),
    Heading(Heading),
    BlockQuote(BlockQuote),
    Admonition(Admonition),
    List(List),
    IndentedCodeBlock(IndentedCodeBlock),
    CodeBlock(CodeBlock),
    MathBlock(MathBlock),
    FootnoteDefinition(FootnoteDefinition),
    Table(Table),
    ThematicBreak(ThematicBreak),
    Interpolation(Interpolation),
    /// A recoverable or future block kind not yet covered by a typed wrapper.
    Other(SyntaxNode),
}

impl Block {
    fn cast(syntax: SyntaxNode) -> Self {
        match syntax.kind() {
            SyntaxKind::PARAGRAPH => Self::Paragraph(Paragraph(syntax)),
            SyntaxKind::ATX_HEADING | SyntaxKind::SETEXT_HEADING => Self::Heading(Heading(syntax)),
            SyntaxKind::BLOCK_QUOTE => Self::BlockQuote(BlockQuote(syntax)),
            SyntaxKind::ADMONITION => Self::Admonition(Admonition(syntax)),
            SyntaxKind::LIST => Self::List(List(syntax)),
            SyntaxKind::INDENTED_CODE_BLOCK => Self::IndentedCodeBlock(IndentedCodeBlock(syntax)),
            SyntaxKind::FENCED_CODE_BLOCK => Self::CodeBlock(CodeBlock(syntax)),
            SyntaxKind::MATH_BLOCK => Self::MathBlock(MathBlock(syntax)),
            SyntaxKind::FOOTNOTE_DEFINITION => Self::FootnoteDefinition(FootnoteDefinition(syntax)),
            SyntaxKind::TABLE => Self::Table(Table(syntax)),
            SyntaxKind::THEMATIC_BREAK => Self::ThematicBreak(ThematicBreak(syntax)),
            SyntaxKind::INTERPOLATION => Self::Interpolation(Interpolation(syntax)),
            _ => Self::Other(syntax),
        }
    }

    /// Return the underlying lossless node.
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Paragraph(node) => node.syntax(),
            Self::Heading(node) => node.syntax(),
            Self::BlockQuote(node) => node.syntax(),
            Self::Admonition(node) => node.syntax(),
            Self::List(node) => node.syntax(),
            Self::IndentedCodeBlock(node) => node.syntax(),
            Self::CodeBlock(node) => node.syntax(),
            Self::MathBlock(node) => node.syntax(),
            Self::FootnoteDefinition(node) => node.syntax(),
            Self::Table(node) => node.syntax(),
            Self::ThematicBreak(node) => node.syntax(),
            Self::Interpolation(node) => node.syntax(),
            Self::Other(node) => node,
        }
    }
}

impl Document {
    /// Iterate top-level blocks in source order.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.syntax().children().map(Block::cast)
    }
}

/// An inline node or semantically relevant inline token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Inline {
    Text(SyntaxToken),
    Escape(SyntaxToken),
    EnDash(SyntaxToken),
    SoftBreak(SyntaxToken),
    Emphasis(Emphasis),
    Strong(Strong),
    Code(InlineCode),
    Math(InlineMath),
    Link(Link),
    Image(Image),
    Autolink(Autolink),
    FootnoteReference(FootnoteReference),
    Interpolation(Interpolation),
    HardBreak(HardBreak),
}

impl Paragraph {
    /// Iterate the paragraph's inline content in source order.
    pub fn inlines(&self) -> impl Iterator<Item = Inline> + '_ {
        inline_children(self.syntax())
    }
}

impl BlockQuote {
    /// Iterate blocks nested below the quote markers.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.syntax().children().map(Block::cast)
    }
}

impl Heading {
    /// Return the heading level from one through six.
    pub fn level(&self) -> u8 {
        let marker = token(self.syntax(), SyntaxKind::HEADING_MARKER)
            .map(|token| token.text().to_string())
            .unwrap_or_default();
        if self.syntax().kind() == SyntaxKind::SETEXT_HEADING {
            if marker.trim_start().starts_with('=') {
                1
            } else {
                2
            }
        } else {
            marker.bytes().filter(|&byte| byte == b'#').count() as u8
        }
    }

    /// Return the visible heading content without its heading delimiters.
    pub fn content(&self) -> String {
        visible_text(self.syntax())
    }

    /// Iterate typed inline content in source order.
    pub fn inlines(&self) -> impl Iterator<Item = Inline> + '_ {
        inline_children(self.syntax())
    }
    /// Return the anchor slug Documenter derives from the heading content.
    ///
    /// Lowercased, with runs of whitespace collapsed to a single hyphen and
    /// every other non-word character dropped. Both the linter's exemption set
    /// and the server's navigation read anchors through this one definition.
    pub fn slug(&self) -> String {
        slug(&self.content())
    }
}

/// Derive a Documenter anchor slug from heading content.
pub fn slug(heading: &str) -> String {
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

impl Admonition {
    /// Return the admonition category following `!!!`.
    pub fn category(&self) -> String {
        token(self.syntax(), SyntaxKind::ADMONITION_CATEGORY)
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }

    /// Return the explicit title, without quotes, or Julia's default title.
    pub fn title(&self) -> String {
        token(self.syntax(), SyntaxKind::ADMONITION_TITLE).map_or_else(
            || uppercase_first(&self.category()),
            |token| token.text().trim_matches('"').to_string(),
        )
    }

    /// Iterate Markdown blocks nested in the admonition body.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.syntax().children().map(Block::cast)
    }
}

impl List {
    /// Iterate list items in source order.
    pub fn items(&self) -> impl Iterator<Item = ListItem> + '_ {
        self.syntax().children().filter_map(ListItem::cast)
    }

    /// Return the first ordinal, or `None` for an unordered list.
    pub fn ordered_start(&self) -> Option<u64> {
        token(self.syntax(), SyntaxKind::LIST_MARKER)?
            .text()
            .trim_end_matches(['.', ')'])
            .parse()
            .ok()
    }

    /// Whether a blank line makes this a loose list in Julia Markdown.
    pub fn is_loose(&self) -> bool {
        self.syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| token.kind() == SyntaxKind::BLANK_LINE)
    }
}

impl ListItem {
    /// Iterate the item's direct inline content.
    pub fn inlines(&self) -> impl Iterator<Item = Inline> + '_ {
        inline_children(self.syntax())
    }

    /// Iterate blocks nested below this item, such as sublists.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.syntax().children().map(Block::cast)
    }
}

impl CodeBlock {
    /// Return the trimmed fence info string.
    pub fn info(&self) -> String {
        token(self.syntax(), SyntaxKind::INFO_STRING)
            .map(|token| token.text().trim().to_string())
            .unwrap_or_default()
    }

    /// Return the code body using Julia Markdown's trailing-newline chomp.
    pub fn content(&self) -> String {
        token(self.syntax(), SyntaxKind::CODE_CONTENT)
            .map(|token| token.text().trim_end_matches(['\r', '\n']).to_string())
            .unwrap_or_default()
    }

    /// Return the exact decoded-text range occupied by the code body.
    pub fn content_range(&self) -> Option<TextRange> {
        token(self.syntax(), SyntaxKind::CODE_CONTENT).map(|token| token.text_range())
    }

    /// Classify ordinary Julia and Documenter fence info strings locally.
    pub fn fence_kind(&self) -> FenceKind {
        classify_fence(&self.info())
    }
}

impl IndentedCodeBlock {
    /// Return code after removing Julia Markdown's four-column block indent.
    pub fn content(&self) -> String {
        let mut out = String::new();
        for token in self
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
        {
            if matches!(token.kind(), SyntaxKind::CODE_CONTENT | SyntaxKind::NEWLINE) {
                out.push_str(token.text());
            }
        }
        out.trim_end_matches(['\r', '\n']).to_string()
    }
}

impl MathBlock {
    /// Return the math formula without block delimiters or its final newline.
    pub fn content(&self) -> String {
        token(self.syntax(), SyntaxKind::MATH_CONTENT)
            .map(|token| token.text().trim_end_matches(['\r', '\n']).to_string())
            .unwrap_or_default()
    }
}

impl FootnoteDefinition {
    /// Return the definition identifier without `[^` and `]:`.
    pub fn id(&self) -> String {
        token(self.syntax(), SyntaxKind::FOOTNOTE_LABEL)
            .map(|token| {
                token
                    .text()
                    .strip_prefix("[^")
                    .and_then(|text| text.strip_suffix("]:"))
                    .unwrap_or(token.text())
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// Iterate Markdown blocks belonging to the footnote body.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.syntax().children().map(Block::cast)
    }
}

impl Table {
    /// Iterate source rows, including the delimiter row at index one.
    pub fn rows(&self) -> impl Iterator<Item = TableRow> + '_ {
        self.syntax().children().filter_map(TableRow::cast)
    }
}

impl TableRow {
    /// Iterate row cells in source order.
    pub fn cells(&self) -> impl Iterator<Item = TableCell> + '_ {
        self.syntax().children().filter_map(TableCell::cast)
    }
}

impl TableCell {
    /// Iterate cell inline content in source order.
    pub fn inlines(&self) -> impl Iterator<Item = Inline> + '_ {
        inline_children(self.syntax())
    }
}

impl InlineCode {
    /// Return the code without its backtick delimiters.
    pub fn content(&self) -> String {
        inline_payload(self.syntax(), SyntaxKind::CODE_CONTENT)
    }
}

impl InlineMath {
    /// Return the formula without its double-backtick delimiters.
    pub fn content(&self) -> String {
        inline_payload(self.syntax(), SyntaxKind::MATH_CONTENT)
    }
}

impl Link {
    /// Return the raw link destination.
    pub fn destination(&self) -> String {
        token(self.syntax(), SyntaxKind::LINK_DESTINATION)
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }

    /// Return the exact decoded-text range of the destination inside `(…)`.
    pub fn destination_range(&self) -> Option<TextRange> {
        token(self.syntax(), SyntaxKind::LINK_DESTINATION).map(|token| token.text_range())
    }

    /// Iterate the link label's inline content.
    pub fn label(&self) -> impl Iterator<Item = Inline> + '_ {
        inline_children(self.syntax())
    }
}

impl Image {
    /// Return the image's alternative text.
    pub fn alt(&self) -> String {
        token(self.syntax(), SyntaxKind::TEXT)
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }

    /// Return the raw image destination.
    pub fn destination(&self) -> String {
        token(self.syntax(), SyntaxKind::LINK_DESTINATION)
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }
}

impl Autolink {
    /// Return the destination between angle brackets.
    pub fn destination(&self) -> String {
        token(self.syntax(), SyntaxKind::LINK_DESTINATION)
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }
}

impl FootnoteReference {
    /// Return the reference identifier without `[^` and `]`.
    pub fn id(&self) -> String {
        token(self.syntax(), SyntaxKind::FOOTNOTE_LABEL)
            .map(|token| {
                token
                    .text()
                    .strip_prefix("[^")
                    .and_then(|text| text.strip_suffix(']'))
                    .unwrap_or(token.text())
                    .to_string()
            })
            .unwrap_or_default()
    }
}

impl Interpolation {
    /// Return the unevaluated Julia expression text after `$`.
    pub fn expression(&self) -> String {
        token(self.syntax(), SyntaxKind::TEXT)
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }
}

/// A statically recognized fenced-code role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceKind {
    Plain,
    Julia,
    JuliaRepl,
    JlDoctest {
        label: Option<String>,
        options: Option<String>,
    },
    Documenter {
        directive: DocumenterDirective,
        arguments: Option<String>,
    },
    DocumenterExtension {
        directive: String,
        arguments: Option<String>,
    },
    Other(String),
}

impl FenceKind {
    /// Whether the fence declares content that Documenter treats as Julia code.
    pub fn contains_julia(&self) -> bool {
        match self {
            Self::Julia | Self::JuliaRepl | Self::JlDoctest { .. } => true,
            Self::Documenter { directive, .. } => !matches!(directive, DocumenterDirective::Raw),
            Self::Plain | Self::DocumenterExtension { .. } | Self::Other(_) => false,
        }
    }
}

/// Core fenced directives documented by Documenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumenterDirective {
    Docs,
    Autodocs,
    Meta,
    Index,
    Contents,
    Example,
    Repl,
    Setup,
    Eval,
    Raw,
}

impl Link {
    /// Return Documenter link metadata for an `@ref`, `@id`, or extension URL.
    pub fn documenter_link(&self) -> Option<DocumenterLink> {
        let destination = token(self.syntax(), SyntaxKind::LINK_DESTINATION)?;
        destination
            .text()
            .trim_start()
            .starts_with('@')
            .then_some(DocumenterLink { destination })
    }
}

/// A Documenter-style link destination retained as source text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocumenterLink {
    destination: SyntaxToken,
}

impl DocumenterLink {
    /// Return the core or extension link kind.
    pub fn kind(&self) -> DocumenterLinkKind<'_> {
        let (name, _) = split_at_word(self.destination.text().trim());
        match name {
            "@ref" => DocumenterLinkKind::Ref,
            "@id" => DocumenterLinkKind::Id,
            other => DocumenterLinkKind::Extension(other.trim_start_matches('@')),
        }
    }

    /// Return an explicit target. `None` means Documenter infers it from the label.
    pub fn target(&self) -> Option<&str> {
        let (_, rest) = split_at_word(self.destination.text().trim());
        (!rest.is_empty()).then_some(rest)
    }

    /// Return the explicit target's decoded-text range.
    pub fn target_range(&self) -> Option<TextRange> {
        let raw = self.destination.text();
        let trimmed_start = raw.len() - raw.trim_start().len();
        let trimmed = raw.trim();
        let name_end = trimmed.find(char::is_whitespace)?;
        let target_start =
            name_end + trimmed[name_end..].len() - trimmed[name_end..].trim_start().len();
        if target_start >= trimmed.len() {
            return None;
        }
        let target_end = trimmed.trim_end().len();
        let base: u32 = self.destination.text_range().start().into();
        Some(TextRange::new(
            TextSize::from(base + (trimmed_start + target_start) as u32),
            TextSize::from(base + (trimmed_start + target_end) as u32),
        ))
    }
}

/// The directive carried by a Documenter-style link destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumenterLinkKind<'a> {
    Ref,
    Id,
    Extension(&'a str),
}

fn classify_fence(info: &str) -> FenceKind {
    if info.is_empty() {
        return FenceKind::Plain;
    }
    if info == "julia" {
        return FenceKind::Julia;
    }
    if info == "julia-repl" {
        return FenceKind::JuliaRepl;
    }
    if info == "jldoctest"
        || info
            .strip_prefix("jldoctest")
            .is_some_and(|rest| rest.starts_with(|ch: char| ch.is_whitespace() || ch == ';'))
    {
        let rest = info.strip_prefix("jldoctest").unwrap().trim();
        let (label, options) = split_label_options(rest);
        return FenceKind::JlDoctest {
            label: nonempty(label),
            options: nonempty(options),
        };
    }
    if let Some(at) = info.strip_prefix('@') {
        let name_end = at
            .find(|ch: char| ch.is_whitespace() || ch == ';')
            .unwrap_or(at.len());
        let name = &at[..name_end];
        let arguments = nonempty(at[name_end..].trim().trim_start_matches(';').trim());
        let directive = match name {
            "docs" => Some(DocumenterDirective::Docs),
            "autodocs" => Some(DocumenterDirective::Autodocs),
            "meta" => Some(DocumenterDirective::Meta),
            "index" => Some(DocumenterDirective::Index),
            "contents" => Some(DocumenterDirective::Contents),
            "example" => Some(DocumenterDirective::Example),
            "repl" => Some(DocumenterDirective::Repl),
            "setup" => Some(DocumenterDirective::Setup),
            "eval" => Some(DocumenterDirective::Eval),
            "raw" => Some(DocumenterDirective::Raw),
            _ => None,
        };
        return match directive {
            Some(directive) => FenceKind::Documenter {
                directive,
                arguments,
            },
            None => FenceKind::DocumenterExtension {
                directive: name.to_string(),
                arguments,
            },
        };
    }
    FenceKind::Other(info.to_string())
}

fn split_label_options(rest: &str) -> (&str, &str) {
    match rest.split_once(';') {
        Some((label, options)) => (label.trim(), options.trim()),
        None => (rest.trim(), ""),
    }
}

fn nonempty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

fn split_at_word(text: &str) -> (&str, &str) {
    match text.find(char::is_whitespace) {
        Some(index) => (&text[..index], text[index..].trim()),
        None => (text, ""),
    }
}

fn token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == kind)
}

fn visible_text(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for token in node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind() {
            SyntaxKind::TEXT | SyntaxKind::CODE_CONTENT | SyntaxKind::MATH_CONTENT => {
                out.push_str(token.text())
            }
            SyntaxKind::ESCAPE => out.push_str(token.text().trim_start_matches('\\')),
            SyntaxKind::EN_DASH => out.push('–'),
            SyntaxKind::SOFT_BREAK => out.push(' '),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn inline_children(node: &SyntaxNode) -> impl Iterator<Item = Inline> + '_ {
    node.children_with_tokens()
        .filter_map(|element| match element {
            rowan::NodeOrToken::Token(token) => match token.kind() {
                SyntaxKind::TEXT => Some(Inline::Text(token)),
                SyntaxKind::ESCAPE => Some(Inline::Escape(token)),
                SyntaxKind::EN_DASH => Some(Inline::EnDash(token)),
                SyntaxKind::SOFT_BREAK => Some(Inline::SoftBreak(token)),
                _ => None,
            },
            rowan::NodeOrToken::Node(node) => match node.kind() {
                SyntaxKind::EMPHASIS => Some(Inline::Emphasis(Emphasis(node))),
                SyntaxKind::STRONG => Some(Inline::Strong(Strong(node))),
                SyntaxKind::INLINE_CODE => Some(Inline::Code(InlineCode(node))),
                SyntaxKind::INLINE_MATH => Some(Inline::Math(InlineMath(node))),
                SyntaxKind::LINK => Some(Inline::Link(Link(node))),
                SyntaxKind::IMAGE => Some(Inline::Image(Image(node))),
                SyntaxKind::AUTOLINK => Some(Inline::Autolink(Autolink(node))),
                SyntaxKind::FOOTNOTE_REFERENCE => {
                    Some(Inline::FootnoteReference(FootnoteReference(node)))
                }
                SyntaxKind::INTERPOLATION => Some(Inline::Interpolation(Interpolation(node))),
                SyntaxKind::HARD_BREAK => Some(Inline::HardBreak(HardBreak(node))),
                _ => None,
            },
        })
}

fn inline_payload(node: &SyntaxNode, kind: SyntaxKind) -> String {
    token(node, kind)
        .map(|token| token.text().trim().to_string())
        .unwrap_or_default()
}

fn uppercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(char::to_uppercase)
        .into_iter()
        .flatten()
        .collect::<String>()
        + chars.as_str()
}
