//! `local-const`: a `const` declaration carrying a `local` modifier. Julia has
//! no such construct, and rejects it at lowering with "syntax: expected
//! assignment after \"const\"" — a message that reads oddly next to `local
//! const z = 1`, where the assignment is plainly there, but the rejection is
//! unconditional. Unlike `const-local`, this rule needs no scope test at all:
//! the top level, a `module` body, a `struct` body, every soft scope and every
//! function body all reject it alike (verified against Julia 1.12).
//!
//! Both modifier orders are flagged — `local const z = 1` and `const local
//! z = 1` mean the same thing — and the span covers the `local` keyword too,
//! since the pair is what Julia rejects.
//!
//! The rule deliberately does *not* cover the neighboring shapes that raise the
//! same Julia message: a bare `const z`, a type-annotated `const z::Int`, and a
//! non-`=` assignment (`const x += 1`). Those are rejected by JuliaSyntax at
//! *parse* time, so the parser already reports them under the `parse-error`
//! pseudo-rule — and a file with a parse diagnostic never reaches the rules at
//! all. `local const z = 1` parses clean in JuliaSyntax and here, which is
//! exactly why it needs a lint rule.
//!
//! Quoted code and macro calls are exempt, as they are for `const-local`:
//! quoted code is data and is never lowered where it is written, and a macro
//! may rewrite what it is handed.
//!
//! No fix is offered: `local z = 1` and `const z = 1` are both plausible
//! repairs and they mean different things.

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::correctness::const_decl;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct LocalConst;

impl Rule for LocalConst {
    fn id(&self) -> &'static str {
        "local-const"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag a `const` declaration carrying a `local` modifier, in either \
         order. Julia has no `local const` construct: the code parses but \
         always fails at lowering with \"expected assignment after `const`\", \
         everywhere — the file top level, a `module` or `struct` body, a loop \
         or `let`, and inside a function alike. Write `local z = 1` for a \
         local binding or `const z = 1` for a constant. A declaration inside \
         quoted code or a macro argument is left alone, since it may never be \
         lowered as written."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`local` and `const` cannot be combined:",
                source: "local const LIMIT = 10\n",
            },
            Example {
                caption: "The other order is the same construct, and a function body is no \
                          different from the top level:",
                source: "function scale(x)\n    const local factor = 2\n    factor * x\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CONST_STMT]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else {
            return;
        };
        let Some(modifier) = const_decl::scope_modifier(node) else {
            return;
        };
        if modifier.kind != SyntaxKind::LOCAL_STMT {
            return;
        }
        if modifier
            .outer
            .ancestors()
            .skip(1)
            .any(|a| const_decl::is_unlowered_context(&a))
        {
            return;
        }

        sink.push(Diagnostic::new(
            self.id(),
            modifier.outer.text_range(),
            "`local const` declaration is not supported",
        ));
    }
}
