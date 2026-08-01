//! `julia-version-compat`: flag syntax that a project's declared Julia support
//! range does not cover. Fatou parses the *superset* of Julia syntax (a
//! permissive parser, see `AGENTS.md`), so a construct introduced in a newer
//! Julia parses cleanly even when the project promises to run on an older one.
//! This rule recovers the "won't run there" signal the permissive parser drops:
//! for each version-gated construct it compares the version that introduced it
//! against the *floor* of the target range (`ctx.julia_target`), and reports
//! when a supported version predates the feature.
//!
//! The target range comes from precedence resolution at the call site (the
//! `--julia-version` flag, `[julia] version`, `Project.toml` `[compat]`, or the
//! manifest's `julia_version`). When no target is known the rule stays silent —
//! there is nothing to check against — so it never fires on a bare file with no
//! project context. This is `Severity::Error`: code using the feature genuinely
//! cannot load on the older supported versions.
//!
//! The feature table is intentionally small and high-confidence, seeded from
//! constructs the parser exposes as distinct node kinds:
//! - `public` declarations ([`PUBLIC_STMT`]) — Julia 1.11.
//! - `import`/`using ... as` renames ([`IMPORT_ALIAS`]) — Julia 1.6.
//!
//! It grows as the parser gains distinct kinds for more version-gated syntax.

use crate::julia_version::Version;
use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

/// One version-gated syntactic construct: the node kind that marks it, the Julia
/// version that introduced it, and how to name it in the diagnostic.
struct Feature {
    kind: SyntaxKind,
    introduced: Version,
    label: &'static str,
}

/// The version-gated constructs this rule knows about. Kept short and certain;
/// extend as the parser exposes more distinctly-kinded syntax.
const FEATURES: &[Feature] = &[
    Feature {
        kind: SyntaxKind::PUBLIC_STMT,
        introduced: Version::new(1, 11, 0),
        label: "the `public` keyword",
    },
    Feature {
        kind: SyntaxKind::IMPORT_ALIAS,
        introduced: Version::new(1, 6, 0),
        label: "renaming with `as` in `import`/`using`",
    },
];

pub struct JuliaVersionCompat;

impl Rule for JuliaVersionCompat {
    fn id(&self) -> &'static str {
        "julia-version-compat"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag syntax newer than the project's declared Julia support range. \
         Fatou parses the full superset of Julia syntax, so a construct from a \
         newer release parses cleanly even when the project targets an older \
         version; this rule reports when a supported version predates the \
         construct (e.g. `public` needs 1.11, `import ... as` needs 1.6). The \
         target range is taken from `--julia-version`, `[julia] version`, or the \
         project's `Project.toml` `[compat]` / `Manifest.toml`; with no target \
         known the rule stays silent."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Targeting Julia 1.0, but `public` needs 1.11 and `as` needs 1.6:",
            source: "module M\npublic foo\nimport A as B\nend\n",
        }]
    }

    fn example_julia_target(&self) -> Option<crate::julia_version::VersionRange> {
        // A 1.0 floor sits below every feature, so the example flags them all.
        Some(crate::julia_version::VersionRange {
            min: Version::new(1, 0, 0),
            max: Some(Version::new(2, 0, 0)),
        })
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        // Every kind named in the feature table.
        &[SyntaxKind::PUBLIC_STMT, SyntaxKind::IMPORT_ALIAS]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // Silent without a declared target: there is nothing to check against.
        let Some(target) = ctx.julia_target else {
            return;
        };
        let Some(node) = el.as_node() else { return };
        let kind = node.kind();
        let Some(feature) = FEATURES.iter().find(|f| f.kind == kind) else {
            return;
        };
        if target.covers_feature(feature.introduced) {
            return;
        }
        sink.push(Diagnostic::new(
            self.id(),
            node.text_range(),
            format!(
                "{} requires Julia {}, but the project supports {} and up",
                feature.label, feature.introduced, target.min
            ),
        ));
    }
}
