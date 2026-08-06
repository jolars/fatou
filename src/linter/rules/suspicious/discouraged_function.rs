//! `discouraged-function`: a call to a function the project would rather not
//! see, matched against a configurable deny-list.
//!
//! The deny-list lives in `[lint.rules.discouraged-function]` (see
//! [`DiscouragedFunctionConfig`]): `functions` replaces the built-in set,
//! `extend-functions` adds to it, and `functions = {}` silences the rule. The
//! built-in set is a small, conservative table of Base/Core functions with
//! process-wide or memory-unsafe effects, each paired with the alternative to
//! reach for.
//!
//! Two shapes are deliberately left alone. A call carrying a trailing `do`
//! block is skipped, because for `cd`, `redirect_stdout`, and `redirect_stderr`
//! the do-block form *is* the recommended alternative — flagging it would be
//! exactly backwards. And a qualified callee (`Base.exit`) spells a different
//! name, so it never matches; the project spelled out a bare name, and asking
//! whether a bare name really is Base's is the namespace gate's job.
//!
//! That gate has two tiers. A built-in name must be confirmed as Base's via
//! [`RuleContext::resolves_to_base`], the same conservative stance every idiom
//! rule takes: unconfirmed means silent, so a file whose `using`s cannot be
//! resolved reports nothing. A project-configured name cannot pass that test —
//! it is by definition not Base's — so it only has to survive
//! [`RuleContext::read_is_shadowed_locally`], which keeps a local of the same
//! name from being reported without making the configuration inert.
//!
//! No fix is offered: the suggestion is prose, and the safe rewrite (threading a
//! value back to the caller instead of `exit`ing, restructuring around a
//! do-block) is a judgment call rather than a mechanical edit.

use crate::ast::AstToken;
use crate::config::DiscouragedFunctionConfig;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext, matchers};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct DiscouragedFunction;

impl Rule for DiscouragedFunction {
    fn id(&self) -> &'static str {
        "discouraged-function"
    }

    fn description(&self) -> &'static str {
        "Flag a call to a function on a configurable deny-list. The built-in \
         set covers Base functions with process-wide or memory-unsafe effects — \
         `exit`, `cd`, `redirect_stdout`, `redirect_stderr`, and the \
         `unsafe_*`/pointer conversions — each reported with the alternative to \
         reach for.\n\n\
         Configure it under `[lint.rules.discouraged-function]`: `functions` \
         replaces the built-in set, `extend-functions` adds to it (an entry \
         there also rewords a built-in), and `functions = {}` silences the rule \
         without ignoring it. Both are tables mapping a function name to the \
         suggestion shown in the diagnostic.\n\n\
         A call carrying a `do` block is never reported, since for `cd` and the \
         `redirect_*` functions that form is the recommended alternative. A \
         qualified callee (`Base.exit`) is a different name and does not match. \
         A built-in name is only reported once it is confirmed to be Base's, so \
         a local of the same name — or a file whose imports cannot be resolved — \
         reports nothing; a name the project configured is reported unless a \
         definition in the same file shadows it. No fix is offered, since the \
         rewrite is a judgment call."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`exit` ends the process and `cd` leaves the working \
                      directory changed:",
            source: "function cleanup()\n    cd(\"/tmp\")\n    exit(1)\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else {
            return;
        };
        let Some(call) = matchers::call_expr(node) else {
            return;
        };
        // The do-block form is the recommended alternative for every built-in
        // entry that has one, so it is never the thing being discouraged.
        if matchers::has_do_block(&call) {
            return;
        }
        let Some(callee) = call.callee_ident() else {
            return;
        };
        let config = &ctx.config.discouraged_function;
        let Some(suggestion) = config.lookup(callee.text()) else {
            return;
        };

        let confirmed = if DiscouragedFunctionConfig::is_builtin(callee.text()) {
            ctx.resolves_to_base(&call)
        } else {
            !ctx.read_is_shadowed_locally(callee.syntax())
        };
        if !confirmed {
            return;
        }

        sink.push(Diagnostic::new(
            self.id(),
            callee.syntax().text_range(),
            format!("`{}` is discouraged: {suggestion}", callee.text()),
        ));
    }
}
