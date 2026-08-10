//! `missing-include-file`: a static `include("path")` whose resolved target is
//! not a file on disk. Running the file throws a `SystemError`, so the finding
//! defaults to an error.
//!
//! The check is driven by the include problems the lint driver precomputes
//! (see [`crate::linter::include_graph`]): the rule itself only matches each
//! problem back to its call site by the literal's decoded path
//! ([`literal_path`]), so it never touches the filesystem. Only statically
//! resolvable includes participate — dynamic, interpolated, qualified, and
//! two-argument forms cannot be resolved without evaluation and are never
//! flagged, and neither is a literal whose escapes denote no path at all. A
//! pathless document (stdin) has no base directory to resolve against and stays
//! silent, and the language server passes no problems (it publishes its own
//! include-graph diagnostics), so no false positive can come from a missing
//! context. A decoded NUL byte can never name a file, so StaticLint's
//! `IncludePathContainsNULL` falls out of the same check. No fix: the linter
//! cannot know the intended file.

use rowan::ast::AstNode;

use crate::ast::CallExpr;
use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::include_graph::IncludeProblemKind;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::project::{include_literal, literal_path};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct MissingIncludeFile;

impl Rule for MissingIncludeFile {
    fn id(&self) -> &'static str {
        "missing-include-file"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag a static `include(\"path\")` whose target does not exist on disk \
         (relative paths resolve against the including file's directory). \
         Running the file would throw a `SystemError`. Only statically \
         resolvable includes are checked: dynamic (`include(f)`), interpolated \
         (`include(\"$dir/a.jl\")`), qualified (`M.include(...)`), and \
         two-argument forms cannot be resolved without running the code and \
         are never flagged."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Including a file that does not exist:",
            source: "include(\"missing.jl\")\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(call) = el.as_node().cloned().and_then(CallExpr::cast) else {
            return;
        };
        let Some(literal) = include_literal(&call) else {
            return;
        };
        let Some(path) = literal_path(&literal) else {
            return;
        };
        let missing = ctx
            .includes
            .iter()
            .any(|problem| problem.kind == IncludeProblemKind::Missing && problem.path == path);
        if missing {
            sink.push(Diagnostic::new(
                self.id(),
                literal.syntax().text_range(),
                format!("included file \"{path}\" does not exist"),
            ));
        }
    }
}
