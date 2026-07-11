---
name: add-lint-rule
description: Add a new built-in lint rule to the fatou linter—implement it
  against the single-walk dispatch, register it in the one source of truth, add
  TDD behavior tests (plus autofix coverage when the rule ships a fix), and
  wire up the snapshot-pinned generated docs and the hand-maintained SUMMARY.md
  index.
---

Use this skill when asked to add a new built-in lint rule (correctness,
suspicious, ...), whether or not it ships an auto-fix. The roadmap of planned
rules lives in `TODO.md` under "Rule roadmap"—when the request names a
roadmap item, follow its category/cost/severity annotation and check the item
off when done. Roadmap items marked "Blocked on future infrastructure" need
that infrastructure first; don't fake it with a heuristic unless the roadmap
entry explicitly sanctions one (e.g. `index-from-length`'s name-based match).

## Tenets that constrain a rule (from `AGENTS.md`)

- **The linter is purely semantic.** Pure layout (spacing, indentation, line
  breaks, operator spelling) is the formatter's job—any check `format
  --check` could perform is **out of scope** for the linter (see the module
  doc of `src/linter/rules.rs`).
- **Parsing is the parser's job.** Do not paper over parser mistakes in a
  rule. If the CST does not expose what you need, extend the typed AST
  wrappers in `src/ast/` (see "AST wrappers" in `AGENTS.md`: `ast_node!`/
  `ast_token!` entry, `support::*` accessors, `Has*` trait impls, re-export
  from `src/ast.rs`, accessor unit test) rather than re-lexing or kind-matching
  raw CST inside the rule.
- **A lint fix is a textual edit, never a formatter.** Applying the fix must
  leave code that still **parses** and stays **lossless** (no misbinding, no
  dropped comments). Make the edit correct *by construction* (tight span,
  whitespace-preserving), or **withhold the fix** for that shape (still report
  the finding). A fix does **not** owe line width or canonical layout—the
  pipeline is fix-then-format and `--fix` never runs the formatter.

## Cost model (drives which infra you may touch)

A rule is **cheap (`syn`)** when it only needs the CST plus typed AST wrappers
and literal inspection; **expensive (`sem`)** when it needs the
`SemanticModel` (`ctx.model`: scopes, bindings, occurrences, imports, module
paths). There is no name-resolution tier yet: anything that must know what an
identifier resolves to (Base/stdlib methods, imported symbols) is **blocked**
until the index/resolution phases land—see the roadmap's blocked section.
Prefer the cheapest tier the rule's correctness actually requires.

## Key files

- `src/linter/rules.rs`—the `Rule` trait, `RuleContext`, and
  **`all_rules()`, the single source of truth**. `all_rule_ids()` derives from
  it, so there is no second list to sync. Every new rule is added here exactly
  once.
- `src/linter/rules/<category>.rs`—the category module
  (`correctness`/`suspicious`; add new categories per the roadmap when first
  needed). Holds `mod <id>;` + `pub use <id>::<Name>;`.
- `src/linter/rules/<category>/<id>.rs`—one file per rule: a unit
  `pub struct` implementing `Rule`, with a module doc comment. (File names are
  snake_case; the public id stays kebab-case.)
- `src/ast/` (`nodes.rs`, `tokens.rs`, `traits.rs`, re-exported from
  `src/ast.rs`)—typed wrappers (`CallExpr`, `IfExpr`, `Condition`,
  `BinaryExpr`, the `Expr` sum, `Ident`/`Operator` tokens, `Has*` traits).
  **Prefer these over raw `children()`/`kind()` matching**; grow them when a
  shape is missing.
- `src/linter/diagnostic.rs`—`Diagnostic`, `Fix`, `Applicability`,
  `Severity`. Build findings with **`Diagnostic::new(id, start, end, message)`**
  and push `Fix { description, content, start, end, applicability }` onto
  `diag.fixes`. **Severity and path are stamped by the engine** after the rule
  runs—rules never set either; override `default_severity()` instead.
- `src/semantic.rs`—`SemanticModel` for `sem`-tier rules (`bindings()`,
  `occurrences()`, `scopes()`, `enclosing_module_path()`, `module_loads()`,
  `free_reads()`, ...).
- `src/linter/suppression.rs`—`# fatou-ignore <rule>` /
  `# fatou-ignore-file <rule>` work for any registered rule automatically;
  nothing to wire per rule.
- `tests/linter_rules.rs`—behavior tests + helpers `findings(rule, src)` /
  `count(rule, src)` (lint with only that rule selected). Each rule gets its
  own `// --- <id> ---` block.
- `tests/autofix.rs`—fix-engine coverage; fixable rules add a `fix_source`
  case here (see workflow step 3).
- `tests/rule_docs.rs`—pins each rule's rendered page via an **`insta`
  snapshot** (`rule_docs_render`), and asserts every rule has a non-empty
  `description()` + at least one `examples()` entry, **and that each example
  actually triggers its own rule**. Accept new snapshots with
  `cargo insta accept`.
- `examples/docgen.rs`—generates `docs/src/reference/rules/<id>.md` **and
  the `docs/src/reference/rules.md` index** from `render_rule_doc`. Run with
  `cargo run --example docgen`. Do not hand-edit either.
- `docs/src/SUMMARY.md`—**hand-maintained**. Add the new rule's line under
  "Lint Rules", matching `all_rules()` order.
- `TODO.md`—the live roadmap ("Rule roadmap" under "Linter"). Check off the
  rule's item with a one-line scope/severity note mirroring the landed
  entries above it.

## Workflow

1. **Pick the rule id (kebab-case).** This is the public `id()`, the
   `--select`/`--ignore`/`[lint.severity]` key, and the docs slug—unique and
   stable; renaming is a breaking change. Match existing tone
   (`unused-binding`, `assignment-in-condition`). The roadmap entry usually
   fixes the id already.

2. **Decide gating and safety before writing code:**
   - **Category** = directory (`correctness`/`suspicious`/...); it does not
     appear in the id.
   - **Severity**—override `default_severity()` (the default is `Warning`;
     `Error` only for code that cannot run, e.g. `duplicate-argument`). Never
     set severity on the `Diagnostic`—the engine stamps it, honoring the
     user's `[lint.severity]` override.
   - **`default_enabled()`**—default `true`; override to `false` for noisy
     opt-in rules (the roadmap flags these).
   - **Dispatch shape**—node-shape rules declare `interests()` (a slice of
     `SyntaxKind`) and implement `check(el, ctx, sink)`, called once per
     matching element in the *one* shared `descendants_with_tokens()` walk
     (tokens included, so token kinds may be subscribed too). Model-driven
     rules leave `interests()` empty and override `check_file(ctx, sink)` (a
     once-per-file pass). **Never walk the whole CST yourself from a
     node-shape rule**; walking *within* the dispatched subtree is fine.
   - **Fix safety**—ship `Applicability::Safe` only when the edit is
     unambiguous and parse-clean by construction; otherwise
     `Applicability::Unsafe` (applied only under `--unsafe-fixes`), or
     withhold the fix entirely and still report.

3. **Write the failing tests first** (TDD per `AGENTS.md`) in
   `tests/linter_rules.rs`:
   - Positive case(s) via `count`/`findings` filtered to the rule.
   - Negative ("should not flag") cases guarding the false positives the
     roadmap entry calls out.
   - If the rule ships a fix: a `fix_source` case in `tests/autofix.rs`
     (snapshot the output, assert `applied` and `remaining.is_empty()`), and
     an `apply_fixes` case if applicability gating matters. Eyeball that the
     fixed output parses clean—there is no automated parse-clean harness yet
     (tracked in `TODO.md`).
   - Run them and watch them fail before implementing.

4. **Implement the rule** in `src/linter/rules/<category>/<id>.rs`:
   - Module doc comment explaining what it flags, why, and any
     false-positive/safety reasoning (see
     `suspicious/assignment_in_condition.rs` for the house style).
   - Cast the dispatched element with the typed wrappers (`el.as_node()` +
     `Foo::cast`, or `el.as_token()`), then use their accessors for shape
     checks.
   - Keep the diagnostic **span tight**—point at the offending construct,
     not the whole statement (it drives the CLI caret and LSP underline).
   - **Required for docs to pass:** implement `description()` (non-empty
     markdown paragraph) and `examples()` (≥1 `Example` whose `source`
     actually triggers the rule and ends with a trailing newline).
   - Trait skeleton:
     ```rust
     impl Rule for MyRule {
         fn id(&self) -> &'static str { "<id>" }
         fn description(&self) -> &'static str { "…" }
         fn examples(&self) -> &'static [Example] { &[Example { caption: "…", source: "…\n" }] }
         fn interests(&self) -> &'static [SyntaxKind] { &[SyntaxKind::…] }
         fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
             // … cast, match shape, then:
             let mut diag = Diagnostic::new(self.id(), start, end, message);
             diag.fixes.push(Fix { … });   // only if fixable
             sink.push(diag);
         }
     }
     ```

5. **Register it** in the single source of truth:
   - `mod <id>;` + `pub use <id>::<Name>;` in
     `src/linter/rules/<category>.rs`.
   - One `Box::new(<category>::<Name>)` line in `all_rules()`
     (`src/linter/rules.rs`), in category order. Nothing else—selection,
     `--select`/`--ignore`/`[lint.severity]` validation, and the docs all
     derive from this list.

6. **Generate and pin the docs:**
   - `cargo run --example docgen` → writes
     `docs/src/reference/rules/<id>.md` and regenerates the `rules.md` index.
   - `cargo test --test rule_docs` will fail on the new snapshot; eyeball the
     `.snap.new` (the example must show the finding and, if fixable, the
     after-fix block), then `cargo insta accept`.
   - **Manually** add the rule's line to `docs/src/SUMMARY.md`—docgen does
     not touch it.

7. **Update `TODO.md`**—check off the roadmap item and add a one-line
   scope/severity note matching the landed entries.

8. **Validate** in order:
   - Targeted: `cargo test --test linter_rules`, `cargo test --test autofix`
     (if fixable), `cargo test --test rule_docs`.
   - Full gates (CI parity): `cargo test`,
     `cargo clippy --all-targets --all-features -- -D warnings`,
     `cargo fmt -- --check`. (`task <name>` wraps these.)

## Dos and don'ts

- **Do** reach for the typed AST wrappers before raw CST walking; grow
  `src/ast/` when a shape is missing (with its own accessor test).
- **Do** keep spans tight and fixes parse-clean/lossless by construction, or
  withhold the fix and still report.
- **Do** make `examples()` snippets that genuinely trigger the rule—the docs
  tests reject a plausible-but-inert example.
- **Don't** set `severity` or `path` on a `Diagnostic`—the engine stamps
  both; override `default_severity()` instead.
- **Don't** add a second registration list or an `if`-guard; `all_rules()` is
  the only list.
- **Don't** implement a formatting/layout preference as a lint rule.
- **Don't** work around a parser/CST gap inside the rule—fix the parser or
  extend the AST wrappers instead.
- **Don't** run the formatter inside a fix, or ship a fix that produces broken
  or lossy code. A fix needn't satisfy line width—that's the formatter's job.
- **Don't** hand-edit `docs/src/reference/rules/<id>.md` or
  `docs/src/reference/rules.md`—regenerate via docgen. (But `SUMMARY.md`
  *is* hand-edited.)

## Report-back format

When done, report:

1. Rule id, category, default severity, and whether it ships a safe/unsafe
   fix (or none).
2. Cost tier (`syn`/`sem`) and `default_enabled`.
3. New files (rule module) and updated files (`<category>.rs`, `rules.rs`
   `all_rules()`, `tests/linter_rules.rs`, `tests/autofix.rs` if fixable, the
   generated `rules/<id>.md` + `rules.md` + accepted snapshot, `SUMMARY.md`,
   `TODO.md`).
4. Targeted test names, including the false-positive guards and (if fixable)
   the `fix_source` case.
5. Full-gate results: `cargo test`, clippy `-D warnings`, `cargo fmt --check`.
