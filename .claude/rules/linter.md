---
paths:
  - "src/linter.rs"
  - "src/linter/**/*.rs"
  - "tests/linter_rules.rs"
  - "tests/autofix.rs"
  - "tests/lint_workspace.rs"
---

# Linter rules

`src/linter/`. **To add a rule, use the `add-lint-rule` skill.** Triaging the
linter against a real Julia codebase: the `linter-investigation` skill. The rule
roadmap is in `TODO.md` ("Rule roadmap").

## Scope

- **The linter is purely semantic.** Anything the formatter's `--check` mode can
  catch belongs to the formatter, not here. Style is never a rule's concern.
- Parse diagnostics **block** linting a file: `check_paths` reports
  `LintStatus::{Clean, Findings, ParseDiagnostics}`.

## Dispatch

- **No rule walks the tree on its own.** Join the driver's single shared walk:
  declare the `SyntaxKind`s you care about via `Rule::interests` and implement
  `Rule::check`. A whole-file rule (semantic-model queries, comment directives)
  leaves `interests` empty and overrides `Rule::check_file`.
- `src/linter/rules.rs`'s `all_rules` is the **single source of truth**;
  `all_rule_ids` derives from it, so there is no second list to keep in sync.
- `ResolvedRules::run` owns **suppression filtering** — it is the only place
  holding both the `# fatou-ignore <rule>[: <reason>]` directive map
  (`suppression.rs`, next-node attachment) and the findings — plus the
  `Rule::check_suppressions` post-pass for facts that only exist after every
  rule has emitted.
- **Severity is stamped by the engine**, like the path: override
  `default_severity`, never set it on the `Diagnostic`.

## Reach for the shared machinery

- Call shape → `rules/matchers.rs`: `plain_call` is the whole "a call to *name*
  with exactly *n* positional arguments and nothing else" opening
  (`plain_broadcast` for `f.(x)`); `CallShape` is the full split when a rule
  needs keywords or has to know which set a splat left open. Never re-derive
  Julia's argument grammar in a rule.
- What a name *means* → `RuleContext`: `resolver()`, `trusts_resolution()`,
  `resolves_to_base()`/`read_resolves_to_base()`, `file_scan()`,
  `control_flow()`. Each is computed once per file and memoized, so asking is
  cheap no matter how many rules do.
- Respelling a construct by splicing its own sub-texts into a new form →
  `rules/rewrite.rs`: `drops_a_comment` (the withhold-the-fix cue) and
  `inline_text` (quotable in a one-line message).
- Cross-file `include()` structure → `include_graph.rs`.
- Which rules need a project-wide resolution context → `RESOLUTION_RULES` in
  `rules.rs`; the CLI's harvest gate and the server's member rule set read that
  one list.

## Rule identity and categories

- A rule `id` is stable kebab-case and **user-visible**: it is the
  `# fatou-ignore` target, the reported rule, and the `select`/`ignore`/
  `[lint.severity]`/`[lint.rules.<id>]` key. Renaming one is a breaking change.
- A rule's **category is its directory and nothing else**. It appears in no
  public surface — not the ID, not any config key, not the generated reference,
  which is one page keyed by ID. Recategorizing is therefore a free refactor.
  Vocabulary: `correctness` (the code cannot do what it says), `suspicious`
  (legal Julia, very likely not intended), `performance` (a rewrite that avoids
  real work), `readability` (a behavior-preserving idiom rewrite), `meta` (a
  finding about a `# fatou-ignore` directive rather than about the Julia).
- Every rule needs a description and `examples()`. The examples are run through
  the **real linter** to render the docs page, so they are behavior, not prose.

## Autofix correctness

**A lint fix is not a formatter.** `lint --fix` applies each fix as a byte-range
replacement (`apply_fixes`, `fix.rs`) and never runs the formatter.

- The bar is **correctness, not layout**. Applying a fix must leave code that
  still parses and is still lossless — never broken syntax, never dropped
  trivia. It must stay locally legible (don't jam tokens together).
- **A fix does not owe line width.** Layout is the formatter's job (Tenet 1) and
  the pipeline is fix-then-format. **Never invoke the formatter from a fix.**
  Run `format` afterward if canonical output is wanted.
- When an edit cannot meet the bar for some shape, make it correct by
  construction (tight span, atom-guarded) or **withhold the fix for that
  shape** — and still report the finding.
- `Safe` fixes apply under `lint --fix`; the rest need `--unsafe-fixes`.

## Config

Per-rule options are typed one field per rule on `RulesConfig`. Strictness is
**deliberately asymmetric**: an unknown ID in `select`/`ignore`/`[lint.severity]`
is *data* the user typed and is only a warning, while `[lint.rules.<id>]` is
*schema* and `deny_unknown_fields` makes a mistyped ID there a config **parse
error**. Options reach rules as `RuleContext::config`, carried on
`ResolvedRules` — keep them off the `run` parameter list.

## Testing and docs

- No fixture directory: add a `#[test]` to `tests/linter_rules.rs` (and
  `tests/autofix.rs` when the rule ships a fix), plus the rule's own
  `examples()`. **Write the failing test first.**
- The rule reference (`docs/src/reference/rules.md`) is **generated** by
  `cargo run --example docgen` and pinned by `tests/rule_docs.rs`. Never
  hand-edit it; a failure there means regenerate, not edit the snapshot.
