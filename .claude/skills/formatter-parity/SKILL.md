---
name: formatter-parity
description: >-
  Grow Fatou's Julia formatter toward JuliaFormatter.jl (DefaultStyle) using the
  differential oracle. rules::lower (src/formatter/rules.rs) lowers the CST into
  the layout IR; the harness in tests/juliaformatter_oracle.rs diffs each
  fixture's format(input) against the pinned expected.jl captured from
  JuliaFormatter. Use this skill to pick the next construct, add its lowering
  rule, lock it with a fixture, and ratchet the now-passing cases into the
  allowlist. Direct parity is the gate; deliberate Tenet-1 divergences are
  recorded in juliaformatter-blocked.txt, never hidden.
---

Use this skill when asked to advance Fatou's formatter, work the
JuliaFormatter-oracle backlog, or "take the next construct." Read `RECAP.md`
first for the latest session, ranked next targets, and traps.

## The oracle in one paragraph

`format(text)` parses to the lossless rowan CST, lowers it via
`rules::lower` (`src/formatter/rules.rs`) into the Wadler/Prettier `Ir`
(`src/formatter/ir.rs`), and prints it with the best-fit engine
(`src/formatter/printer.rs`). The gate is **direct parity**:
`format(input) == juliaformatter(input)`, where
`expected.jl = JuliaFormatter.format_text(input)` (DefaultStyle) is pinned per
fixture. One corpus, pinned (no Julia at test time → CI-safe):

- **Dir corpus** `tests/fixtures/formatter/<slug>/` (`input.jl` + `expected.jl`,
  the latter minted by `scripts/update-juliaformatter-corpus.sh`), version-pinned
  in `tests/fixtures/formatter/.juliaformatter-source`. Gated by
  `juliaformatter_allowlist`; every slug is accounted for in
  `tests/oracle/juliaformatter-allowlist.txt` **or**
  `tests/oracle/juliaformatter-blocked.txt` (the latter with a one-line rationale)
  — enforced by `juliaformatter_corpus_fully_triaged`. The report's FAIL bucket is
  the backlog.

The report (`tests/oracle/juliaformatter-report.txt`) is gitignored and
regenerated.

## What this skill is NOT

- **Not a JuliaFormatter clone.** JuliaFormatter `DefaultStyle` is largely
  **preservation**-oriented: it leaves the user's operator/assignment spacing
  (it normalizes neither `a*b` nor `a * b`; same for `=`, `==`, `&&`, `->`) and
  numeric literals as written. Tenet 1 forbids Fatou from being non-deterministic,
  so Fatou picks one canonical form and **records** the divergence in
  `juliaformatter-blocked.txt`. Diverging is allowed but must raise tension: a
  documented decision, never silent (`AGENTS.md`). After the Runic→JuliaFormatter
  flip, this preservation mismatch is the single biggest source of divergences —
  expect many "Fatou canonicalizes; JuliaFormatter preserves" entries.
- **Not a pass-rate chase.** Every handled construct must stay **idempotent**
  (`format(format(x)) == format(x)`; guarded by `tests/formatter.rs` over all
  fixtures) and must never mangle unhandled syntax—that's what the transparent
  fallback protects.
- **Not semantic rewriting.** Layout rules only; semantic rewrites (e.g.
  return-insertion) are deferred. (Note: unlike Runic, JuliaFormatter
  `DefaultStyle` does **not** insert `return`, so that particular rewrite is moot
  for this target.)

## Failure buckets (classify before fixing)

- **Missing rule**: Fatou lowers the construct transparently (verbatim), so
  JuliaFormatter's reshaping isn't reproduced. Add a `lower_*` arm. This is the
  bulk of the backlog (e.g. `for x in` → `for x =`).
- **Wrong rule**: a `lower_*` arm produces the wrong spacing/break (often a
  Runic-shaped rule that now diverges, e.g. spaced `{A, B}` vs JuliaFormatter's
  tight `{A,B}`). Fix the arm; probe JuliaFormatter for the exact target first.
- **Tenet-1 divergence**: JuliaFormatter preserves user whitespace/literal form;
  Fatou canonicalizes deterministically. Record in `juliaformatter-blocked.txt`
  with a rationale.
- **Semantic rewrite**: deferred; blocked if it ever surfaces.
- **Upstream (parser/lexer) blocker**: the construct never tokenizes/parses
  cleanly, so Fatou can only bail to transparent (it sees ERROR nodes, not the
  real shape). **Not fixable in this skill**: `rules.rs` is the only growth
  surface here. Keep the broken shape out of the fixture (use a parser-safe
  variant), and **hand the gap off** (see the workflow's conclusion step) so it
  reaches the `parser-parity` skill.

## The rule recipe

A construct touches just `src/formatter/rules.rs`:

1. Add a `match` arm in `lower_node` for the node `SyntaxKind`, dispatching to a
   `lower_<construct>` helper (split into a `rules/<construct>.rs` submodule
   alongside `rules.rs` once the file grows—no `mod.rs`).
2. Build the `Ir`: `Ir::text` for tokens, `Ir::concat` to sequence, `Ir::Line`/
   `Ir::group`/`Ir::indent` for breakable layout. Recurse into operand/child nodes
   via `lower_node` so handled descendants keep normalizing.
3. **Bail to `lower_transparent` on any shape you don't fully model** (interleaved
   comment/newline, error recovery, missing child). Never guess—a verbatim
   passthrough is always idempotent and lossless.

The mechanism is tool-neutral; only the *target shape* changed with the oracle
flip. Always probe JuliaFormatter for the exact target before writing a rule —
many landed rules were shaped to Runic and now need flipping.

## Workflow (per session)

1. **Read `RECAP.md`** (traps, latest session, ranked next targets). Prefer a
   user-named target.
2. **Baseline**: `cargo test`—note it's green. "No regression" = still green.
3. **Probe JuliaFormatter** for the exact target shape (write snippets to a temp
   file to avoid shell quoting traps with `"`/`$`/`'`):
   ```sh
   julia --startup-file=no -e 'import JuliaFormatter; print(JuliaFormatter.format_text("CODE"; margin=92, indent=4))'
   ```
   Probe both spacings (`a OP b` and `aOPb`)—if JuliaFormatter preserves the
   input, it's a Tenet-1 divergence (canonicalize + block), not a normalization
   rule.
4. **Classify** into a bucket, then apply the **smallest** rule. Inspect the CST
   via `cargo run -q -- parse <file>` and the output via
   `cargo run -q -- format <file>` (note: `format <file>` rewrites in place; pipe
   via stdin to print to stdout).
5. **TDD fixture**—add `tests/fixtures/formatter/<name>/input.jl` (constructs
   this rule should bring to parity; keep deferred shapes out so it can be
   allowlisted). Mint `expected.jl`:
   ```sh
   bash scripts/update-juliaformatter-corpus.sh   # needs devenv julia; writes every expected.jl + .juliaformatter-source
   diff <(cargo run -q -- format < tests/fixtures/formatter/<name>/input.jl) \
        tests/fixtures/formatter/<name>/expected.jl   # expect identical for a PASS
   ```
6. **Re-triage + reseed the allowlist**. Regenerate the report, then keep the
   header and replace the slug list with the current PASS set:
   ```sh
   cargo test --test juliaformatter_oracle -- --ignored juliaformatter_full_report
   { grep -E '^#|^$' tests/oracle/juliaformatter-allowlist.txt; \
     grep '^PASS' tests/oracle/juliaformatter-report.txt | awk '{print $2}' | sort; } \
     > /tmp/jal && mv /tmp/jal tests/oracle/juliaformatter-allowlist.txt
   ```
   Move the now-passing slug out of `juliaformatter-blocked.txt`, and add any
   genuine new divergence there with a rationale (else
   `juliaformatter_corpus_fully_triaged` goes red). Confirm the pass count went
   **up** (or held) and divergence didn't rise.
7. **Guardrails**:
   ```sh
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   cargo fmt -- --check
   ```
8. **Update `TODO.md`** (mark a formatter bullet `[x]`/trim the backlog) and
   **`RECAP.md`**.
9. **Hand off any upstream (parser/lexer) blocker you hit.** Formatter work
   routinely surfaces gaps that aren't yours to fix—a construct that won't
   tokenize or parse, leaving you only the transparent bail. Don't let it die in
   this skill's RECAP. Record it where the fixer will look:
   - a **"Queued next target"** note at the top of
     `.claude/skills/parser-parity/RECAP.md`, and/or a bullet under `TODO.md`'s
     **Parser** section;
   - include the **JuliaSyntax ground truth** (`julia --startup-file=no -e 'using
     JuliaSyntax; print(JuliaSyntax.parse(Expr, "CODE"))'`), what Fatou does
     instead, and the crux (e.g. a maximal-munch interaction);
   - cross-reference it from this skill's `RECAP.md` and from the formatter
     fixture that had to route around it.
10. **Commit.** Conventional Commits; subject ≤ 60 chars. New layout capability =
   `feat(formatter)`; test-infra-only = `test(formatter)`; a handoff/RECAP-only
   change = `docs(...)`. The pre-commit hook runs clippy + rustfmt—never
   `--no-verify`. Don't push unless asked.

## Session boundaries

A committed target with `RECAP.md` updated is a clean stop—`RECAP.md` is the
handoff. One construct per context window is the intended cadence; the rolling log
exists so you don't have to span more than one target in a context.

## Key files

- `src/formatter/rules.rs`: `lower`/`lower_node`/`lower_transparent`, the
  per-construct rules. The growth surface.
- `src/formatter/ir.rs`: the `Ir` primitives (`Text`/`Concat`/`Line`/`SoftLine`/
  `HardLine`/`Indent`/`Group`).
- `src/formatter/printer.rs`: best-fit layout engine (group flat-vs-break).
- `src/formatter/core.rs`: `format`/`format_with_style` entry points.
- `tests/juliaformatter_oracle.rs`: harness (allowlist gate, triage, full-coverage check).
- `tests/formatter.rs`: idempotence invariant over all fixtures.
- `scripts/update-juliaformatter-corpus.{sh,jl}`: regen pinned `expected.jl` + `.juliaformatter-source`.

## Traps

- **Reseeding must preserve the allowlist header.** Use the `grep -E '^#|^$'`
  recipe; don't clobber the comment block.
- **Report is gitignored; `expected.jl` is generated**—never hand-edit
  `expected.jl` (regenerate via the script), never commit
  `juliaformatter-report.txt`.
- **`format <file>` rewrites in place.** Pipe via stdin to inspect output without
  touching the fixture.
- **JuliaFormatter DefaultStyle preserves much of the source** (operator/
  assignment spacing, numeric literals, `∈` in comprehensions). Probe both
  spacings before writing a normalization rule—a preserved operator is a Tenet-1
  divergence to block, not a rule to write. Many landed rules are Runic-shaped and
  now diverge.
- **Corpus pinned** to JuliaFormatter in `.juliaformatter-source`. A bump ⇒ re-run
  `scripts/update-juliaformatter-corpus.sh`, re-triage.
- **Always bail to transparent on an unmodeled shape.** Idempotence
  (`tests/formatter.rs`) and losslessness of unhandled syntax depend on it.

## Report-back format

1. Construct landed (e.g. "operator spacing").
2. Corpus: pass/divergence before → after (regressions: must be zero).
3. Allowlist count before → after; new blocked entries (with rationale).
4. Files changed, by failure bucket.
5. Ranked next target. If ending uncommitted/with regressions, say so and list the
   red tests.
6. Any **upstream (parser/lexer) blocker** surfaced, and where it was handed off
   (parser-parity RECAP/`TODO.md`). "None" if clean.
