use std::collections::BTreeSet;

use fatou_parser::parser::to_juliasyntax_sexpr;
use rowan::NodeOrToken;

use super::{Ir, PreservedComment, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
use SyntaxKind::*;

pub(crate) struct Selectors {
    head: SyntaxNode,
    keyword: SyntaxToken,
    items: Vec<Item>,
    reordered: bool,
}

enum Item {
    Name {
        node: SyntaxNode,
        local: String,
        alias_source: Option<String>,
        leading: Vec<PreservedComment>,
        trailing: Vec<PreservedComment>,
    },
    Divider(PreservedComment),
}

impl Selectors {
    pub(crate) fn read(node: &SyntaxNode, trailing: Option<&SyntaxToken>) -> Option<Self> {
        if !matches!(node.kind(), USING_STMT | IMPORT_STMT) {
            return None;
        }
        let mut elements = node.children_with_tokens();
        let keyword = elements.next()?.into_token()?;
        let mut significant = elements.by_ref().filter(|el| el.kind() != WHITESPACE);
        let head = significant.next()?.into_node()?;
        if head.kind() != IMPORT_PATH
            || significant.next()?.kind() != COLON
            || node
                .ancestors()
                .any(|ancestor| matches!(ancestor.kind(), QUOTE_EXPR | QUOTE_SYM | MACRO_CALL))
            || head
                .descendants_with_tokens()
                .any(|el| matches!(el.kind(), ERROR | INTERPOLATION))
        {
            return None;
        }

        let mut items = Vec::new();
        let mut expect_name = true;
        let mut inline = false;
        let mut pending_blocks = Vec::new();
        for element in elements {
            match element {
                NodeOrToken::Node(name) if expect_name => {
                    let (local, alias_source) = binding_names(&name)?;
                    items.push(Item::Name {
                        node: name,
                        local,
                        alias_source,
                        leading: std::mem::take(&mut pending_blocks),
                        trailing: Vec::new(),
                    });
                    expect_name = false;
                    inline = true;
                }
                NodeOrToken::Token(token) => match token.kind() {
                    WHITESPACE => {}
                    NEWLINE => {
                        attach_after_name(&mut items, &mut pending_blocks, inline);
                        inline = false;
                    }
                    COMMA if !expect_name => expect_name = true,
                    BLOCK_COMMENT if expect_name => {
                        // An inline block before the next name belongs to that
                        // name. A newline can instead close a suffix or divider.
                        pending_blocks.push(PreservedComment::from_token(&token));
                    }
                    COMMENT | BLOCK_COMMENT => {
                        pending_blocks.push(PreservedComment::from_token(&token));
                        attach_after_name(&mut items, &mut pending_blocks, inline);
                    }
                    _ => return None,
                },
                _ => return None,
            }
        }
        if expect_name || !matches!(items.last(), Some(Item::Name { .. })) {
            return None;
        }
        if let Some(token) = trailing {
            pending_blocks.push(PreservedComment::from_token(token));
            attach_after_name(&mut items, &mut pending_blocks, true);
        }

        // Stable sorting preserves the order of clauses that bind the same
        // local name. Standalone comments delimit independent runs.
        let mut reordered = false;
        for run in items.split_mut(|item| matches!(item, Item::Divider(_))) {
            if !run.is_sorted_by(|a, b| a.local() <= b.local()) && !has_alias_dependency(run) {
                run.sort_by(|a, b| a.local().cmp(b.local()));
                reordered = true;
            }
        }

        let selectors = Self {
            head,
            keyword,
            items,
            reordered,
        };
        if selectors.ends_with_line_comment() && has_output_suppressor(node) {
            return None;
        }
        Some(selectors)
    }

    pub(crate) fn ends_with_line_comment(&self) -> bool {
        matches!(self.items.last(), Some(Item::Name { trailing, .. })
            if trailing.last().is_some_and(|comment| comment.is_line))
    }

    pub(crate) fn lower(&self) -> Ir {
        let mut names = Vec::new();
        let commented = self
            .items
            .iter()
            .any(|item| item.comment_counts() != [0; 3]);
        for (index, item) in self.items.iter().enumerate() {
            names.push(if commented { Ir::HardLine } else { Ir::Line });
            match item {
                Item::Name {
                    node,
                    leading,
                    trailing,
                    ..
                } => {
                    for comment in leading {
                        names.push(comment.own_line_ir());
                        names.push(Ir::text(" "));
                    }
                    names.push(super::lower_node(node));
                    if index + 1 < self.items.len() {
                        names.push(Ir::text(","));
                    }
                    for comment in trailing {
                        names.push(comment.trailing_ir());
                    }
                }
                Item::Divider(comment) => names.push(comment.own_line_ir()),
            }
        }
        Ir::group(Ir::concat([
            Ir::text(self.keyword.text()),
            Ir::text(" "),
            super::lower_node(&self.head),
            Ir::text(":"),
            Ir::indent(Ir::concat(names)),
        ]))
    }

    /// Reorder the original clauses and comments for verification without
    /// running the layout rule on either side of the comparison.
    pub(crate) fn reordered_source(&self) -> Option<String> {
        if !self.reordered {
            return None;
        }

        let mut source = format!("{} {}:", self.keyword.text(), self.head.text());
        for (index, item) in self.items.iter().enumerate() {
            source.push('\n');
            match item {
                Item::Name {
                    node,
                    leading,
                    trailing,
                    ..
                } => {
                    for comment in leading {
                        source.push_str(&comment.text);
                        source.push(' ');
                    }
                    source.push_str(&node.text().to_string());
                    if index + 1 < self.items.len() {
                        source.push(',');
                    }
                    for comment in trailing {
                        source.push(' ');
                        source.push_str(&comment.text);
                    }
                }
                Item::Divider(comment) => source.push_str(&comment.text),
            }
        }
        Some(source)
    }

    /// The verifier compares both comment text and the sorted positions that
    /// own it. A divider occupies an item position of its own.
    pub(crate) fn comment_positions(&self) -> impl Iterator<Item = (usize, [usize; 3])> + '_ {
        self.items.iter().enumerate().filter_map(|(index, item)| {
            let counts = item.comment_counts();
            (counts != [0; 3]).then_some((index, counts))
        })
    }
}

impl Item {
    fn local(&self) -> &str {
        match self {
            Self::Name { local, .. } => local,
            Self::Divider(_) => unreachable!("dividers split the sortable runs"),
        }
    }

    fn comment_counts(&self) -> [usize; 3] {
        match self {
            Self::Name {
                leading, trailing, ..
            } => [leading.len(), trailing.len(), 0],
            Self::Divider(_) => [0, 0, 1],
        }
    }
}

// With a self-import, an alias can supply another clause's source name.
fn has_alias_dependency(run: &[Item]) -> bool {
    let sources: BTreeSet<_> = run
        .iter()
        .filter_map(|item| match item {
            Item::Name { alias_source, .. } => alias_source.as_deref(),
            Item::Divider(_) => None,
        })
        .collect();
    !sources.is_empty()
        && run.iter().any(|item| {
            matches!(item, Item::Name { local, alias_source: Some(_), .. }
                if sources.contains(local.as_str()))
        })
}

fn attach_after_name(items: &mut Vec<Item>, comments: &mut Vec<PreservedComment>, inline: bool) {
    match items.last_mut() {
        Some(Item::Name { trailing, .. }) if inline => trailing.append(comments),
        _ => items.extend(comments.drain(..).map(Item::Divider)),
    }
}

/// A top-level semicolon without a following statement suppresses the value
/// echo. A new final line comment must not cover it.
fn has_output_suppressor(node: &SyntaxNode) -> bool {
    node.parent()
        .is_some_and(|p| p.kind() == TOPLEVEL_SEMICOLON)
        && node
            .siblings_with_tokens(rowan::Direction::Next)
            .skip(1)
            .find(|el| el.kind() != WHITESPACE)
            .is_some_and(|el| el.kind() == SEMICOLON)
        && node.next_sibling().is_none()
}

pub(crate) fn has_same_line_suffix(node: &SyntaxNode, comment: Option<&SyntaxToken>) -> bool {
    let mut next = comment.cloned().or_else(|| node.last_token());
    while let Some(token) = next.and_then(|token| token.next_token()) {
        match token.kind() {
            WHITESPACE => next = Some(token),
            NEWLINE => return false,
            _ => return true,
        }
    }
    false
}

pub(crate) fn following_comment(node: &SyntaxNode) -> Option<SyntaxToken> {
    if !matches!(node.kind(), USING_STMT | IMPORT_STMT) {
        return None;
    }
    let mut siblings = node.siblings_with_tokens(rowan::Direction::Next).skip(1);
    let mut token = siblings.find(|el| el.kind() != WHITESPACE)?.into_token()?;
    // A block's trailing semicolon is removed by body lowering. Carry its
    // comment along too, so a second pass sees the same attachment.
    if token.kind() == SEMICOLON && node.parent().is_some_and(|p| p.kind() == BLOCK) {
        token = siblings.find(|el| el.kind() != WHITESPACE)?.into_token()?;
    }
    if token.kind() == COMMENT
        || token.kind() == BLOCK_COMMENT
            && siblings
                .find(|el| el.kind() != WHITESPACE)
                .is_none_or(|el| el.kind() == NEWLINE)
    {
        Some(token)
    } else {
        None
    }
}

fn binding_names(node: &SyntaxNode) -> Option<(String, Option<String>)> {
    // An interpolated selector is syntax data, and a dotted selector can refer
    // to a binding introduced by another clause. Leave those lists in order.
    let path = match node.kind() {
        IMPORT_PATH => node.clone(),
        IMPORT_ALIAS => node.first_child()?,
        _ => return None,
    };
    let mut parts = path
        .children_with_tokens()
        .filter(|el| !matches!(el.kind(), LPAREN | RPAREN | WHITESPACE));
    let part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let original = name_part(&part, &path)?;
    if node.kind() == IMPORT_PATH {
        return Some((original, None));
    }

    let alias = node.last_child_or_token()?;
    let local = match alias {
        NodeOrToken::Token(token) if token.kind() == IDENT => {
            if token.text().is_ascii() {
                Some(token.text().to_owned())
            } else {
                let projected = to_juliasyntax_sexpr(node, &[]);
                Some(projected.strip_suffix(')')?.rsplit_once(' ')?.1.to_owned())
            }
        }
        NodeOrToken::Node(name) if name.kind() == MACRO_NAME => macro_name(&name, node),
        _ => None,
    }?;
    let source = (local != original).then_some(original);
    Some((local, source))
}

fn name_part(part: &SyntaxElement, path: &SyntaxNode) -> Option<String> {
    match part {
        NodeOrToken::Token(token) if token.kind() == IDENT => {
            if token.text().is_ascii() {
                Some(token.text().to_owned())
            } else {
                let projected = to_juliasyntax_sexpr(path, &[]);
                Some(
                    projected
                        .strip_prefix("(importpath ")?
                        .strip_suffix(')')?
                        .to_owned(),
                )
            }
        }
        NodeOrToken::Token(token) if token.kind().is_operator() => {
            // A leading dot denotes a relative path in this token form.
            (!token.text().starts_with('.')).then(|| folded_operator(token.text()))
        }
        NodeOrToken::Node(name) if name.kind() == OPERATOR_ATOM => {
            let token = name.first_token()?;
            Some(folded_operator(token.text()))
        }
        NodeOrToken::Node(name) if name.kind() == MACRO_NAME => macro_name(name, path),
        NodeOrToken::Node(name) if name.kind() == NONSTANDARD_IDENTIFIER => var_name(name),
        _ => None,
    }
}

fn var_name(node: &SyntaxNode) -> Option<String> {
    let projected = to_juliasyntax_sexpr(node, &[]);
    if projected == "(var)" {
        return Some(String::new());
    }
    Some(
        projected
            .strip_prefix("(var ")?
            .strip_suffix(')')?
            .to_owned(),
    )
}

fn macro_name(node: &SyntaxNode, clause: &SyntaxNode) -> Option<String> {
    if let Some(name) = node.first_child()
        && name.kind() == NONSTANDARD_IDENTIFIER
    {
        return Some(format!("@{}", var_name(&name)?));
    }
    let projected = to_juliasyntax_sexpr(clause, &[]);
    let name = projected
        .strip_suffix("))")?
        .rsplit_once(" (macro_name ")?
        .1;
    Some(format!("@{name}"))
}

fn folded_operator(text: &str) -> String {
    text.replace(['\u{b7}', '\u{387}'], "⋅").replace('−', "-")
}
