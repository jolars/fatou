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
//! A call carrying a trailing `do` block is skipped, because for `cd`,
//! `redirect_stdout`, and `redirect_stderr` the callback form restores the
//! previous state. Passing that callback explicitly as the first of two
//! arguments is equivalent. Positional splats leave the overload unknown, so
//! these three names are also skipped when their arguments are splatted.
//! A qualified callee (`Base.exit`) spells a different name, so it never
//! matches; the project spelled out a bare name, and asking whether a bare name
//! really is Base's is the namespace gate's job.
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
        "Report calls to functions with process-wide or unsafe effects. Defaults include `exit`, \
         `cd`, stream redirection, and unsafe memory operations. Each diagnostic suggests an \
         alternative. Callback forms of `cd` and stream redirection are allowed because they \
         restore the previous state.\n\n\
         Configure `[lint.rules.discouraged-function]` with tables mapping function names to \
         suggestions. `functions` replaces the defaults, `extend-functions` adds or overrides \
         entries, and `functions = {}` clears the list. Qualified calls such as `Base.exit` are \
         not checked."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`cd` changes the working directory, and `exit` ends the process:",
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
        if matches!(callee.text(), "cd" | "redirect_stdout" | "redirect_stderr") {
            let shape = matchers::CallShape::of(&call);
            // Both spellings of the callback overload restore the prior state;
            // a splat may supply that callback even when it is not visible.
            if shape.positional.len() == 2 || shape.positional_open {
                return;
            }
        }

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
