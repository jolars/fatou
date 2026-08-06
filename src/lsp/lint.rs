//! Lint findings as LSP diagnostics.
//!
//! Runs the linter with the rule set the main loop resolved for the document
//! (discovered `fatou.toml` shadowing editor-pushed settings, see
//! `crate::lsp::config`) and converts each finding into an LSP diagnostic: the
//! rule ID as `code`, the engine-stamped severity mapped across, and
//! `source: "fatou"` like the parse diagnostics it is published alongside.
//! Rules only run on a parse-clean tree (the CLI's rule), so the analysis
//! pipeline gates on empty parse diagnostics before calling in here.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString, Range};

use crate::config::LintConfig;
use crate::incremental::Analysis;
use crate::linter::rules::ResolutionContext;
use crate::linter::{self, ResolvedRules, Severity, all_rules, lint_parsed};
use crate::parser::parse;
use crate::semantic::SemanticModel;
use crate::text::{LineIndex, PositionEncoding};

/// Lint `text` off the snapshot's cached parse and semantic model when the
/// db's tracked buffer for `path` still matches it; otherwise re-parse. A
/// write racing the read trips `salsa::Cancelled`, which also falls back to a
/// fresh parse. Returns nothing on a parse-broken buffer: rules need a clean
/// tree, and the parse errors are published by the caller already.
pub(crate) fn lint_diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
    rules: &ServerRules,
) -> Vec<Diagnostic> {
    findings_to_lsp(
        lint_findings_via_db(snapshot, path, text, rules),
        text,
        encoding,
    )
}

/// Compute the lint diagnostics for `text` with the default rules, re-parsing
/// it. The pure core of the lint-diagnostic pipeline; empty on a parse-broken
/// document.
pub fn compute_lint_diagnostics(text: &str, encoding: PositionEncoding) -> Vec<Diagnostic> {
    findings_to_lsp(
        lint_findings(text, &ServerRules::defaults()),
        text,
        encoding,
    )
}

/// The raw lint findings for `text`, warm off the snapshot's cached parse and
/// semantic model (see [`lint_diagnostics_via_db`] for the cache contract).
/// Raw so code actions can reach the byte-ranged [`linter::Fix`]es a finding
/// carries, which the LSP diagnostic conversion drops.
pub(crate) fn lint_findings_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    rules: &ServerRules,
) -> Vec<linter::Diagnostic> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        if !snapshot.parse_diagnostics(file).is_empty() {
            return Some(Vec::new());
        }
        let root = snapshot.parsed_tree(file);
        let model = snapshot.semantic_model(file);
        // The server resolves free reads against its harvested library, with
        // the workspace tier when the file belongs to the package under
        // development. `undefined-name` joins the rule set only then: for a
        // workspace member the include graph pins the host module, so sibling
        // and host globals resolve; a loose file may be an `include`d fragment
        // whose host we cannot know.
        let workspace = snapshot.workspace_member(path);
        let rules = rules.get(workspace.is_some());
        let resolution = Some(ResolutionContext {
            packages: snapshot,
            workspace,
        });
        // No include problems: the server publishes its own include-graph
        // diagnostics (see `crate::lsp::graph_diagnostics`), so the
        // include-graph lint rules stay silent here.
        Some(lint_parsed(
            Some(path),
            &root,
            model,
            rules,
            resolution,
            &[],
        ))
    }));
    match cached {
        Ok(Some(findings)) => findings,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => lint_findings(text, rules),
    }
}

/// The raw lint findings for `text`, re-parsing it; empty on a parse-broken
/// document (rules need a clean tree). No resolution context: this cold path
/// serves a racing write, and inventing undefined-name findings without the
/// library would flash false positives — missing them for one round trip is
/// the safe failure.
fn lint_findings(text: &str, rules: &ServerRules) -> Vec<linter::Diagnostic> {
    let parsed = parse(text);
    if !parsed.diagnostics.is_empty() {
        return Vec::new();
    }
    let model = SemanticModel::build(&parsed.cst);
    lint_parsed(None, &parsed.cst, &model, rules.get(false), None, &[])
}

/// The rules that are sound only with the resolution context a workspace
/// member file carries: the server adds them to the member rule set, while
/// the CLI leaves them opt-in via `--select`.
const WORKSPACE_MEMBER_RULES: &[&str] = &["undefined-name", "call-arity"];

/// The rule sets the server lints with, resolved once per configuration (the
/// dispatch table included) rather than per lint run: the configured set
/// as-is, plus a variant with [`WORKSPACE_MEMBER_RULES`] added for workspace
/// member files, where the server carries the resolution context that makes
/// those rules sound.
pub(crate) struct ServerRules {
    plain: ResolvedRules,
    member: ResolvedRules,
}

impl ServerRules {
    /// Resolve both rule sets from `lint`, returning the unknown rule IDs the
    /// configuration named (for the caller to log). The member variant unions
    /// [`WORKSPACE_MEMBER_RULES`] into the effective enabled set before
    /// resolving, so `ignore` still subtracts afterward (a user who ignores
    /// `call-arity` keeps it off even for members) and `severity` overrides
    /// carry through.
    pub(crate) fn from_config(lint: &LintConfig) -> (Self, Vec<String>) {
        let (plain, unknown) = ResolvedRules::resolve(lint);
        let mut select = match &lint.select {
            Some(select) => select.clone(),
            None => all_rules()
                .iter()
                .filter(|rule| rule.default_enabled())
                .map(|rule| rule.id().to_string())
                .collect(),
        };
        for rule in WORKSPACE_MEMBER_RULES {
            if !select.iter().any(|id| id == rule) {
                select.push(rule.to_string());
            }
        }
        let member_config = LintConfig {
            select: Some(select),
            ignore: lint.ignore.clone(),
            severity: lint.severity.clone(),
            rules: lint.rules.clone(),
        };
        // Unknown IDs are reported off the plain resolve only: the member
        // config repeats the user's IDs, so its unknowns are duplicates.
        let (member, _) = ResolvedRules::resolve(&member_config);
        (Self { plain, member }, unknown)
    }

    pub(crate) fn get(&self, workspace_member: bool) -> &ResolvedRules {
        if workspace_member {
            &self.member
        } else {
            &self.plain
        }
    }

    /// The default-configuration rule sets, shared by the cold fallback paths
    /// and tests.
    pub(crate) fn defaults() -> Arc<ServerRules> {
        static DEFAULTS: LazyLock<Arc<ServerRules>> =
            LazyLock::new(|| Arc::new(ServerRules::from_config(&LintConfig::default()).0));
        Arc::clone(&DEFAULTS)
    }
}

fn findings_to_lsp(
    findings: Vec<linter::Diagnostic>,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let line_index = LineIndex::new(text);
    findings
        .into_iter()
        .map(|finding| finding_to_lsp(&finding, &line_index, encoding))
        .collect()
}

/// Convert one lint finding into an LSP diagnostic against `line_index`'s text
/// (the source the finding's byte offsets index).
pub(crate) fn finding_to_lsp(
    finding: &linter::Diagnostic,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Diagnostic {
    // The `unused-` rule family flags dead code; the tag lets clients render
    // those findings faded rather than squiggled.
    let tags = finding
        .rule
        .starts_with("unused-")
        .then(|| vec![DiagnosticTag::UNNECESSARY]);
    Diagnostic {
        range: Range::new(
            line_index.byte_to_position(finding.range.start().into(), encoding),
            line_index.byte_to_position(finding.range.end().into(), encoding),
        ),
        severity: Some(severity_to_lsp(finding.severity)),
        code: Some(NumberOrString::String(finding.rule.to_string())),
        source: Some("fatou".to_string()),
        message: finding.message.body.clone(),
        tags,
        ..Default::default()
    }
}

fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use lsp_types::Position;

    const UNUSED_LOCAL: &str = "function f(x)\n    tmp = x + 1\n    return x\nend\n";

    #[test]
    fn unused_binding_becomes_a_tagged_warning() {
        let diags = compute_lint_diagnostics(UNUSED_LOCAL, PositionEncoding::Utf16);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("unused-binding".to_string()))
        );
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(diag.source.as_deref(), Some("fatou"));
        assert_eq!(diag.tags, Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(
            diag.range,
            Range::new(Position::new(1, 4), Position::new(1, 7)),
            "the diagnostic must cover `tmp`"
        );
        assert!(diag.message.contains("tmp"));
    }

    #[test]
    fn non_unused_rules_are_untagged() {
        let diags = compute_lint_diagnostics("if x = 1\nend\n", PositionEncoding::Utf16);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String(
                "assignment-in-condition".to_string()
            ))
        );
        assert_eq!(diags[0].tags, None);
    }

    #[test]
    fn suppression_comments_are_honored() {
        let suppressed = "function f(x)\n    # fatou-ignore unused-binding\n    tmp = x + 1\n    return x\nend\n";
        assert_eq!(
            compute_lint_diagnostics(suppressed, PositionEncoding::Utf16),
            Vec::new()
        );
    }

    #[test]
    fn a_parse_broken_document_yields_no_lint_findings() {
        // The unterminated function also contains an unused local; rules must
        // not run on the error-recovered tree.
        let broken = "function f(x)\n    tmp = x + 1\n    return x\n";
        assert_eq!(
            compute_lint_diagnostics(broken, PositionEncoding::Utf16),
            Vec::new()
        );
    }

    /// A workspace member file gets the `undefined-name` rule: siblings from
    /// the package index resolve, a typo is flagged. A non-member file (no
    /// workspace context) does not run the rule at all.
    #[test]
    fn undefined_name_runs_only_for_workspace_members() {
        use super::super::cross_file::test_support::{member_path, workspace_db};

        let src = "f() = helper() + helprr()\n";
        let (db, _) = workspace_db(&["helper"], &[("a.jl", src)]);
        let diags = lint_diagnostics_via_db(
            &db.snapshot(),
            &member_path("a.jl"),
            src,
            PositionEncoding::Utf16,
            &ServerRules::defaults(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("undefined-name".to_string()))
        );
        assert!(diags[0].message.contains("helprr"));

        // The same source outside any workspace: no undefined-name findings.
        let path = Path::new("/work/loose.jl");
        let mut plain = IncrementalDatabase::default();
        plain.upsert_file(path, src.to_string());
        assert_eq!(
            lint_diagnostics_via_db(
                &plain.snapshot(),
                path,
                src,
                PositionEncoding::Utf16,
                &ServerRules::defaults()
            ),
            Vec::new()
        );
    }

    /// The cached-tree lint path matches the re-parse path when the db's
    /// tracked buffer is the live text, and falls back (still correctly) when
    /// the db lags the buffer or has never seen the path.
    #[test]
    fn lint_via_db_matches_compute_and_falls_back() {
        let encoding = PositionEncoding::Utf16;
        let path = Path::new("/work/a.jl");
        let expected = compute_lint_diagnostics(UNUSED_LOCAL, encoding);
        assert_eq!(expected.len(), 1, "fixture must produce a finding");

        // Cache hit: tracked text == buffer → lint off the cached tree + model.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, UNUSED_LOCAL.to_string());
        assert_eq!(
            lint_diagnostics_via_db(
                &db.snapshot(),
                path,
                UNUSED_LOCAL,
                encoding,
                &ServerRules::defaults()
            ),
            expected,
            "cached-tree lint must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            lint_diagnostics_via_db(
                &stale.snapshot(),
                path,
                UNUSED_LOCAL,
                encoding,
                &ServerRules::defaults()
            ),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            lint_diagnostics_via_db(
                &empty.snapshot(),
                path,
                UNUSED_LOCAL,
                encoding,
                &ServerRules::defaults()
            ),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
