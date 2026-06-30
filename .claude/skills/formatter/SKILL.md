---
name: formatter
description: >-
  Grow Fatou's own Julia formatter, one construct at a time, against
  hand-authored fixtures. rules::lower (src/formatter/rules.rs) lowers the CST
  into the layout IR (ir.rs) printed by printer.rs; the gate in tests/formatter.rs
  diffs each fixture's format(input.jl) against a hand-written expected.jl. There
  is no external reference formatter: you propose a formatting, the user edits
  expected.jl to the desired form, you push back if it breaks a tenet or conflicts
  with an existing rule, then you implement the rule. Parser/lexer blockers stop
  and hand off to parser-parity.
---

Use this skill when asked to advance Fatou's formatter or "take the next
construct." Read `RECAP.md` first for the latest session, the rule inventory, and
the traps.

## The formatter in one paragraph

`format(text)` parses to the lossless rowan CST, lowers it via `rules::lower`
(`src/formatter/rules.rs`) into the Wadler/Prettier `Ir` (`src/formatter/ir.rs`),
and prints it with the best-fit engine (`src/formatter/printer.rs`). Fatou owns
its style — there is **no external reference formatter** (we used to track
Runic.jl; that target is gone). The gate is hand-authored fixtures:
`tests/fixtures/formatter/<slug>/` holds `input.jl` plus a hand-written
`expected.jl`, and `tests/formatter.rs` asserts `format(input.jl) == expected.jl`.
**Presence of `expected.jl` is gate membership** — a fixture with only `input.jl`
is a construct still being authored, not yet gated. No allowlist, no blocked list.

## The two invariants (`tests/formatter.rs`)

- **Gate** (`formatter_fixtures_match_expected`): every fixture with an
  `expected.jl` must format to it exactly.
- **Stability** (`formatter_is_idempotent_and_stable`): over **every** `input.jl`
  (gated or not), `format(format(x)) == format(x)` and the output parses cleanly.
  This guards against mangling any curated input as rules land.

## The authoring workflow (per construct)

This is a **human-in-the-loop** loop. You do not invent the canonical form alone;
you propose, the user decides, you implement.

1. **Read `RECAP.md`** (traps, latest session, rule inventory, ranked targets).
   Prefer a user-named target. Baseline: `cargo test` is green.
2. **Surface candidate inputs and propose a formatting.** Pick a small set of
   representative `input.jl` snippets for the construct. Inspect the CST
   (`cargo run -q -- parse <file>`) and current output
   (`cargo run -q -- format < file`). Propose the `expected.jl` you believe is
   right **under Tenet 1** (deterministic full reflow, input-independent), and
   explain the reasoning. Hand it to the user.
3. **The user hand-edits `expected.jl`** to the form they want.
4. **Push back when warranted.** If the user's choice is unprincipled, breaks a
   tenet (especially Tenet 1 determinism), or conflicts with an existing rule or
   another fixture, **raise friction**: name the conflict, show the affected
   rule/fixture, and resolve it together before writing code. Diverging from a
   prior decision is allowed but must be conscious and recorded, never silent.
5. **Implement the rule.** Add a `lower_<construct>` arm (recipe below) in
   `rules.rs`; touch `ir.rs`/`printer.rs` only if the layout genuinely needs a new
   primitive. Preserve the **bail-to-transparent** discipline.
6. **Lock it.** Commit the agreed `input.jl` + `expected.jl` under
   `tests/fixtures/formatter/<slug>/`. The gate goes green for that slug.
7. **Guardrails:**
   ```sh
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   cargo fmt -- --check
   ```
   Stability (idempotence + clean reparse) must hold.
8. **Parser/lexer blocker => STOP and hand off.** (See below.)
9. **Update `TODO.md`** (mark a formatter bullet) and **`RECAP.md`**. Commit
   (Conventional Commits; subject <= 60 chars; `feat(formatter)` for a new layout
   capability, `test(formatter)` for fixtures-only, `docs(...)` for a RECAP/handoff
   change). The pre-commit hook runs clippy + rustfmt — never `--no-verify`. Don't
   push unless asked.

## The rule recipe

A construct touches just `src/formatter/rules.rs`:

1. Add a `match` arm in `lower_node` for the node `SyntaxKind`, dispatching to a
   `lower_<construct>` helper (split into a `rules/<construct>.rs` submodule once
   the file grows — no `mod.rs`).
2. Build the `Ir`: `Ir::text` for tokens, `Ir::concat` to sequence,
   `Ir::Line`/`Ir::group`/`Ir::indent` for breakable layout. Recurse into
   operand/child nodes via `lower_node` so handled descendants keep normalizing.
3. **Bail to `lower_transparent` on any shape you don't fully model** (interleaved
   comment/newline, error recovery, missing child). Never guess — a verbatim
   passthrough is always idempotent and lossless.

## Tenet 1 is the authority

Output is decided solely by the formatter's rules and the layout engine, never by
how the input was written. Semantically-equivalent inputs **must** format
identically: source line breaks, whitespace, operator spelling (`in` vs `∈`,
`a*b` vs `a * b`), and numeric-literal form never influence the result. Fatou
**fully reflows**, laying out each construct from scratch under `line_width`,
breaking only where width or semantics require it. Push back against hard-coding
special cases for specific constructs. (See AGENTS.md.)

## What this skill is NOT

- **Not source-break mirroring.** Author `expected.jl` as the canonical
  fully-reflowed form. **Do not** let the current code's output define
  `expected.jl` — much of today's machinery still mirrors source breaks (a legacy
  of the old Runic target), which Tenet 1 forbids. That is the trap to avoid; the
  real reflow engine is the headline future target (see RECAP).
- **Not a pass-rate chase.** Every handled construct stays idempotent and must
  never mangle unhandled syntax — that's what the transparent fallback protects.
- **Not semantic rewriting.** Layout only; semantic rewrites (e.g. implicit
  `return` insertion) are out of scope.

## Parser/lexer blockers — stop and hand off

Formatter work routinely surfaces gaps that aren't yours to fix — a construct that
won't tokenize or parse cleanly, leaving you only the transparent bail (you see
ERROR nodes, not the real shape). `rules.rs` is the only growth surface here.
**Stop**, keep the broken shape out of the fixture (use a parser-safe variant),
and record the gap where the fixer will look:

- a **"Queued next target"** note at the top of
  `.claude/skills/parser-parity/RECAP.md`, and/or a bullet under `TODO.md`'s
  **Parser** section;
- include the **JuliaSyntax ground truth**
  (`julia --startup-file=no -e 'using JuliaSyntax; print(JuliaSyntax.parse(Expr, "CODE"))'`),
  what Fatou does instead, and the crux;
- cross-reference it from this skill's `RECAP.md` and from the fixture that had to
  route around it.

## Key files

- `src/formatter/rules.rs`: `lower`/`lower_node`/`lower_transparent`, the
  per-construct rules. The growth surface.
- `src/formatter/ir.rs`: the `Ir` primitives (`Text`/`Concat`/`Line`/`SoftLine`/
  `HardLine`/`BlankLine`/`Indent`/`Group`).
- `src/formatter/printer.rs`: best-fit layout engine (group flat-vs-break).
- `src/formatter/core.rs`: `format`/`format_with_style` entry points.
- `tests/formatter.rs`: the gate + stability invariants.

## Traps

- **`expected.jl` is hand-authored** — never capture it from any formatter,
  including Fatou's current output. Author it to the canonical full-reflow form.
- **`format <file>` rewrites in place.** Pipe via stdin
  (`cargo run -q -- format < file`) to inspect output without touching a fixture.
- **Always bail to transparent on an unmodeled shape.** Stability (idempotence +
  clean reparse) and losslessness of unhandled syntax depend on it.
- **A parser/lexer gap is not yours to patch in `rules.rs`** — hand it off.

## Session boundaries

A committed construct with `RECAP.md` updated is a clean stop — `RECAP.md` is the
handoff. One construct per context window is the intended cadence.

## Report-back format

1. Construct landed (e.g. "operator spacing").
2. Gate: fixtures passing before -> after (regressions must be zero).
3. Files changed.
4. Any **parser/lexer blocker** surfaced and where it was handed off
   (parser-parity RECAP / `TODO.md`). "None" if clean.
5. Ranked next target. If ending uncommitted or with regressions, say so and list
   the red tests.
