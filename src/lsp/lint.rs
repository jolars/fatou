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
use std::str::FromStr;
use std::sync::{Arc, LazyLock};

use lsp_types::{
    CodeDescription, Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString, Range, Uri,
};

use crate::config::LintConfig;
use crate::incremental::Analysis;
use crate::linter::docs::rule_doc_url;
use crate::linter::rules::{RESOLUTION_RULES, ResolutionContext, is_shipped_rule};
use crate::linter::{self, ResolvedRules, Severity, lint_parsed};
use crate::parser::parse;
use crate::resolve::PackageSource;
use crate::semantic::SemanticModel;
use crate::text::{LineIndex, PositionEncoding, TextBuffer};

/// Lint `text` off the snapshot's cached parse and semantic model when the
/// db's tracked buffer for `path` still matches it; otherwise re-parse. A
/// write racing the read trips `salsa::Cancelled`, which also falls back to a
/// fresh parse. Returns nothing on a parse-broken buffer: rules need a clean
/// tree, and the parse errors are published by the caller already.
pub(crate) fn lint_diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
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
    text: &TextBuffer,
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
        // development. Workspace membership and explicit script programs pin
        // the host module, so sibling and host globals resolve. Loose files
        // require an explicit opt-in because their host is unknown.
        let scripts =
            (!rules.entry_points.is_empty()).then(|| snapshot.scripts(&rules.entry_points));
        let in_script = scripts
            .as_ref()
            .is_some_and(|scripts| scripts.applies(path));
        let workspace = if in_script {
            None
        } else {
            snapshot.workspace_member(path)
        };
        let script_packages = ScriptPackages(snapshot);
        let rules = if in_script {
            &rules.script
        } else {
            rules.get(workspace.is_some())
        };
        // Only a member file loads against the package's own project, so the
        // declared dependency set (and with it `unresolved-import`) rides along
        // exactly there.
        let declared_deps = workspace
            .as_ref()
            .and_then(|(pkg, _)| snapshot.declared_deps(&pkg.name));
        let resolution = Some(ResolutionContext {
            packages: if scripts.is_some() {
                &script_packages
            } else {
                snapshot
            },
            workspace,
            declared_deps,
        });
        // No include problems: the server publishes its own include-graph
        // diagnostics (see `crate::lsp::graph_diagnostics`), so the
        // include-graph lint rules stay silent here.
        Some(crate::linter::check::lint_parsed_with_scripts(
            Some(path),
            &root,
            model,
            rules,
            resolution,
            &[],
            scripts.as_ref(),
        ))
    }));
    match cached {
        Ok(Some(findings)) => findings,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => lint_findings(text, rules),
    }
}

/// A standalone editor can have no workspace harvest at all. Scripts still
/// need the same built-in Base/Core floor as the CLI while indexing catches up.
struct ScriptPackages<'a>(&'a Analysis);

impl PackageSource for ScriptPackages<'_> {
    fn package(&self, name: &str) -> Option<Arc<crate::index::PackageIndex>> {
        self.0
            .package(name)
            .or_else(|| crate::linter::check::system_snapshot().get(name).cloned())
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

/// The rules whose full behavior needs the resolution context a workspace
/// member file carries. The server adds any default-off entries to the member
/// rule set; default-on entries are already present but stay in this single
/// authoritative list for resolution gating and suppression handling.
const WORKSPACE_MEMBER_RULES: &[&str] = RESOLUTION_RULES;

/// The rule sets the server lints with, resolved once per configuration (the
/// dispatch table included) rather than per lint run: the configured set
/// as-is, a variant with [`WORKSPACE_MEMBER_RULES`] added for workspace members,
/// and a variant with script defaults for explicit programs.
pub(crate) struct ServerRules {
    pub(crate) entry_points: Vec<std::path::PathBuf>,
    plain: ResolvedRules,
    member: ResolvedRules,
    script: ResolvedRules,
}

impl ServerRules {
    /// Resolve the rule sets from `lint`, returning the unknown rule IDs the
    /// configuration named (for the caller to log). The member variant unions
    /// [`WORKSPACE_MEMBER_RULES`] into the effective enabled set before
    /// resolving, so `ignore` still subtracts afterward (a user who ignores
    /// `call-arity` keeps it off even for members) and `severity` overrides
    /// carry through.
    pub(crate) fn from_config(lint: &LintConfig) -> (Self, Vec<String>) {
        let (plain, unknown) = ResolvedRules::resolve(lint);
        let mut member_config = lint.clone();
        member_config
            .extend_select
            .extend(WORKSPACE_MEMBER_RULES.iter().map(|id| id.to_string()));
        // Report unknown IDs from the plain resolve only: the other variants
        // repeat the user's IDs and would duplicate those warnings.
        let (member, _) = ResolvedRules::resolve(&member_config);
        let (script, _) = ResolvedRules::resolve_for_scripts(lint);
        (
            Self {
                plain,
                member,
                script,
                entry_points: Vec::new(),
            },
            unknown,
        )
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
        // Turns the rule code into a link to its reference section in clients
        // that support it. Only for registry rules: `parse-error` and friends
        // have no section to point at.
        code_description: is_shipped_rule(finding.rule)
            .then(|| Uri::from_str(&rule_doc_url(finding.rule)).ok())
            .flatten()
            .map(|href| CodeDescription { href }),
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
    #[test]
    fn script_entry_uses_unsaved_sibling_globals() {
        use crate::incremental::IncrementalDatabase;
        use crate::script_loading::load_sources;
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let helper = dir.path().join("helper.jl");
        std::fs::write(&entry, "provided = 1\ninclude(\"helper.jl\")\n").unwrap();
        let text = "f() = provided + typo\n";
        std::fs::write(&helper, text).unwrap();
        let entries = vec![entry.clone()];
        let mut db = IncrementalDatabase::default();
        db.set_script_sources(
            &entries,
            &load_sources(&entries, &BTreeMap::new()),
            &BTreeMap::new(),
        );
        let (mut rules, _) = super::ServerRules::from_config(&crate::config::LintConfig::default());
        rules.entry_points = entries;
        let buffer = crate::text::TextBuffer::new(text);
        let check = |db: &IncrementalDatabase| {
            super::lint_findings_via_db(&db.snapshot(), &helper, &buffer, &rules)
        };
        let findings = check(&db);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].message.body.contains("`typo`"));
        db.upsert_file(&entry, "typo = 1\ninclude(\"helper.jl\")\n");
        let findings = check(&db);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].message.body.contains("`provided`"));
    }

    use super::*;
    use crate::incremental::IncrementalDatabase;
    use lsp_types::Position;

    const UNUSED_LOCAL: &str = "function f(x)\n    tmp = x + 1\n    return x\nend\n";

    #[test]
    fn explicitly_enabled_undefined_names_explain_includes_without_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.jl");
        let mut db = IncrementalDatabase::default();
        let config = LintConfig {
            select: Some(vec!["undefined-name".into()]),
            severity: [("undefined-name".into(), Severity::Error)].into(),
            ..Default::default()
        };
        let (rules, _) = ServerRules::from_config(&config);
        let text = TextBuffer::from("include()\nf() = typo\n");
        db.upsert_file(&path, text.text_arc());
        let diagnostics = lint_diagnostics_via_db(
            &db.snapshot(),
            &path,
            &text,
            PositionEncoding::Utf16,
            &rules,
        );
        let [notice] = &diagnostics[..] else {
            panic!("expected one notice: {diagnostics:?}");
        };
        assert_eq!(notice.severity, Some(DiagnosticSeverity::INFORMATION));
        assert_eq!(
            notice.code,
            Some(NumberOrString::String("analysis-incomplete".into()))
        );
        assert_eq!(
            notice.range,
            Range::new(Position::new(0, 0), Position::new(0, 9))
        );
        assert!(notice.message.contains("entry-points"));
        assert!(notice.code_description.is_none());
        assert!(lint_findings(text.text(), &rules).is_empty());

        let repaired = TextBuffer::from("f() = typo\n");
        db.upsert_file(&path, repaired.text_arc());
        let diagnostics = lint_diagnostics_via_db(
            &db.snapshot(),
            &path,
            &repaired,
            PositionEncoding::Utf16,
            &rules,
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(NumberOrString::String("undefined-name".into()))
        );
    }

    #[test]
    fn script_defaults_honor_selection_ignore_and_severity() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let loose = dir.path().join("loose.jl");
        let text = TextBuffer::from("f() = typo\n");
        let mut db = IncrementalDatabase::default();
        db.set_script_sources(
            std::slice::from_ref(&entry),
            &[(entry.clone(), Ok(Arc::from(text.text())))].into(),
            &Default::default(),
        );
        db.upsert_file(&loose, text.text_arc());

        for (config, expected) in [
            (LintConfig::default(), Some(Severity::Warning)),
            (
                LintConfig {
                    select: Some(vec![]),
                    ..Default::default()
                },
                None,
            ),
            (
                LintConfig {
                    select: Some(vec!["unused-binding".into()]),
                    ..Default::default()
                },
                None,
            ),
            (
                LintConfig {
                    ignore: vec!["undefined-name".into()],
                    ..Default::default()
                },
                None,
            ),
            (
                LintConfig {
                    extend_select: vec!["unused-argument".into()],
                    ..Default::default()
                },
                Some(Severity::Warning),
            ),
            (
                LintConfig {
                    severity: [("undefined-name".into(), Severity::Hint)].into(),
                    ..Default::default()
                },
                Some(Severity::Hint),
            ),
        ] {
            let (mut rules, _) = ServerRules::from_config(&config);
            rules.entry_points = vec![entry.clone()];
            let findings = lint_findings_via_db(&db.snapshot(), &entry, &text, &rules);
            assert_eq!(
                findings.len(),
                usize::from(expected.is_some()),
                "{config:?}: {findings:?}"
            );
            if let Some(severity) = expected {
                assert_eq!(findings[0].rule, "undefined-name");
                assert_eq!(findings[0].severity, severity);
            }
            assert!(lint_findings_via_db(&db.snapshot(), &loose, &text, &rules).is_empty());
        }
    }

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
    fn a_finding_carries_a_link_to_its_reference_section() {
        let diags = compute_lint_diagnostics(UNUSED_LOCAL, PositionEncoding::Utf16);
        let href = diags[0]
            .code_description
            .as_ref()
            .expect("finding should carry a code description")
            .href
            .as_str()
            .to_string();
        assert_eq!(
            href,
            "https://fatou.dev/reference/rules.html#unused-binding"
        );
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
            &TextBuffer::new(src.to_string()),
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
                &TextBuffer::new(src.to_string()),
                PositionEncoding::Utf16,
                &ServerRules::defaults()
            ),
            Vec::new()
        );
    }

    #[test]
    fn extend_select_preserves_default_diagnostics_for_loose_and_member_files() {
        use super::super::cross_file::test_support::{member_path, workspace_db};

        let src = "function f(x, spare)\n    unused = x\n    return missing_name\nend\n";
        let config = LintConfig {
            extend_select: vec!["undefined-name".to_string(), "unused-argument".to_string()],
            severity: [("undefined-name".to_string(), Severity::Hint)].into(),
            ..Default::default()
        };
        let (rules, unknown) = ServerRules::from_config(&config);
        assert!(unknown.is_empty());
        let (member_db, _) = workspace_db(&[], &[("a.jl", src)]);
        let dir = tempfile::tempdir().unwrap();
        let plain_path = dir.path().join("loose.jl");
        let mut plain_db = IncrementalDatabase::default();
        plain_db.upsert_file(&plain_path, src.to_string());

        for (db, path) in [(plain_db, plain_path), (member_db, member_path("a.jl"))] {
            let diags = lint_diagnostics_via_db(
                &db.snapshot(),
                &path,
                &TextBuffer::new(src.to_string()),
                PositionEncoding::Utf16,
                &rules,
            );
            let mut codes: Vec<_> = diags
                .iter()
                .map(|diag| match diag.code.as_ref().unwrap() {
                    NumberOrString::String(id) => id.as_str(),
                    NumberOrString::Number(_) => panic!("expected a rule ID"),
                })
                .collect();
            codes.sort_unstable();
            assert_eq!(
                codes,
                ["undefined-name", "unused-argument", "unused-binding"]
            );
            assert_eq!(
                diags
                    .iter()
                    .find(|diag| diag.code == Some(NumberOrString::String("undefined-name".into())))
                    .unwrap()
                    .severity,
                Some(DiagnosticSeverity::HINT)
            );
        }
    }

    #[test]
    fn extend_select_honors_ignore_in_both_server_rule_sets() {
        let config = LintConfig {
            select: Some(vec![]),
            extend_select: vec![
                "undefined-name".to_string(),
                "unused-argument".to_string(),
                "future-rule".to_string(),
            ],
            ignore: vec!["undefined-name".to_string()],
            ..Default::default()
        };
        let (rules, unknown) = ServerRules::from_config(&config);
        assert_eq!(unknown, ["future-rule"]);
        for member in [false, true] {
            let enabled = rules.get(member).enabled();
            assert!(enabled.contains("unused-argument"));
            assert!(!enabled.contains("undefined-name"));
            assert!(!enabled.contains("unused-binding"));
        }
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
                &TextBuffer::new(UNUSED_LOCAL.to_string()),
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
                &TextBuffer::new(UNUSED_LOCAL.to_string()),
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
                &TextBuffer::new(UNUSED_LOCAL.to_string()),
                encoding,
                &ServerRules::defaults()
            ),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
