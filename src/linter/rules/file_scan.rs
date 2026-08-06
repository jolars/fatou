//! The per-file soundness scan the resolution-dependent rules share.
//!
//! Julia's `include`-splicing and metaprogramming make "what does this name
//! mean?" undecidable for a file in isolation. Every rule that asks the
//! [`Resolver`](crate::resolve::Resolver) a question first has to know whether
//! the file is answerable at all, and which spans inside it are trustworthy.
//! One pass over the CST collects both: the `eval`/`include` shapes that bail
//! the whole file, and the macro-call and quote extents that exempt a span.
//!
//! Built once per file and memoized on [`RuleContext`](super::RuleContext) —
//! ask for it with [`RuleContext::file_scan`](super::RuleContext::file_scan)
//! rather than running the walk again.

use rowan::TextRange;

use crate::ast::{AstNode, AstToken, CallExpr, MacroCall};
use crate::project::include_target;
use crate::syntax::{SyntaxKind, SyntaxNode};

/// One pass over the CST collecting everything a resolution-dependent rule
/// skips or bails on: macro-call and quote extents, and the `eval`/`include`
/// call shapes.
pub(crate) struct FileScan {
    /// `MACRO_CALL` extents. A macro receives unevaluated expressions and may
    /// rewrite them or bind names itself, so what is written inside one is not
    /// what runs. The macro's own name is still a real read.
    macro_calls: Vec<TextRange>,
    /// `QUOTE_EXPR`/`QUOTE_SYM` extents: quoted code is data, not code.
    quotes: Vec<TextRange>,
    /// The file calls `eval`/`@eval`, which can define anything at runtime.
    pub(crate) calls_eval: bool,
    /// The file `include`s a literal path. The harvest follows those, so they
    /// are only a problem without a workspace to have harvested them.
    pub(crate) literal_include: bool,
    /// The file `include`s a computed path, which no harvest can follow.
    pub(crate) dynamic_include: bool,
}

impl FileScan {
    pub(crate) fn collect(root: &SyntaxNode) -> Self {
        let mut scan = FileScan {
            macro_calls: Vec::new(),
            quotes: Vec::new(),
            calls_eval: false,
            literal_include: false,
            dynamic_include: false,
        };
        for node in root.descendants() {
            match node.kind() {
                SyntaxKind::MACRO_CALL => {
                    scan.macro_calls.push(node.text_range());
                    let name = MacroCall::cast(node)
                        .and_then(|call| call.name())
                        .and_then(|name| name.macro_token());
                    if name.is_some_and(|token| token.text() == "eval") {
                        scan.calls_eval = true;
                    }
                }
                SyntaxKind::QUOTE_EXPR | SyntaxKind::QUOTE_SYM => {
                    scan.quotes.push(node.text_range());
                }
                SyntaxKind::CALL_EXPR => {
                    let Some(call) = CallExpr::cast(node) else {
                        continue;
                    };
                    let Some(callee) = call.callee_ident() else {
                        continue;
                    };
                    match callee.text() {
                        "eval" => scan.calls_eval = true,
                        "include" => {
                            if include_target(&call).is_some() {
                                scan.literal_include = true;
                            } else {
                                scan.dynamic_include = true;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        scan
    }

    /// Whether `range` sits inside quoted code (`:(…)`, `:sym`, `quote … end`).
    pub(crate) fn in_quote(&self, range: TextRange) -> bool {
        within(&self.quotes, range)
    }

    /// Whether `range` sits inside a macro call's extent, the macro's own name
    /// included.
    pub(crate) fn in_macro_call(&self, range: TextRange) -> bool {
        within(&self.macro_calls, range)
    }

    /// Whether the code at `range` is exempt outright: quoted, or inside a
    /// macro call. The composition every rule reasoning about *code* wants;
    /// `undefined-name`, which reasons about individual *reads*, keeps the
    /// macro's own name in scope and so composes the two halves itself.
    pub(crate) fn in_skipped(&self, range: TextRange) -> bool {
        self.in_quote(range) || self.in_macro_call(range)
    }
}

fn within(extents: &[TextRange], range: TextRange) -> bool {
    extents.iter().any(|e| e.contains_range(range))
}
