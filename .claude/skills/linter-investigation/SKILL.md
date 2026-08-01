---
name: linter-investigation
description: Investigate fatou's linter (and, secondarily, its parser) against a
  real-world Julia codebase. Clone a target repo, lint it, and triage the
  diagnostics for false positives, incorrect spans, and unsafe autofixes; parse
  failures on valid Julia are caught along the way. Suspected bugs are confirmed
  against the JuliaSyntax differential oracle (and `julia` if available) before
  being called bugs. Use when asked to stress-test, investigate, or triage the
  linter (or parser) over an external repo or corpus.
---

Point fatou's linter at a large body of real Julia code and hunt for **linter
quality bugs**: false positives, incorrect spans, and unsafe fixes. This is the
primary goal. **Parse failures are a secondary catch**—a parse error blocks
linting a file, so they surface naturally, and a parse failure on *valid* Julia
is a real parser bug worth reporting—but the center of gravity is the linter,
not a full parser audit.

This is **distinct from the `smoke-test-triage` skill.** That one reacts to the
weekly automated corpus scan's *formatter* regressions (losslessness,
idempotence, format-error, panic) filed as GitHub issues. This skill is
proactive and interactive: you choose a repo and go looking for linter/parser
quality problems. Formatter losslessness and idempotence are out of scope
here—leave them to `smoke-test-triage`.

## The core principle (read first)

**A finding is only a bug once you can show fatou is wrong.** Real codebases
contain *intentionally* invalid files (parser regression inputs, error-recovery
fixtures), so a diagnostic on those is correct, not a bug. Classify each
suspicious finding into exactly one of:

- **True positive** — fatou is right; move on.
- **False positive** — fatou flags legitimate Julia. The highest-value find.
- **Incorrect span** — the finding is real but the caret underlines the wrong
  tokens.
- **Unsafe fix** — the autofix produces code that doesn't parse, changes
  semantics, or drops trivia (a comment). Test the fix, don't eyeball it.
- **Parser bug** — valid Julia that fatou fails to parse or mis-parses (surfaces
  as a syntax diagnostic or a wrong CST shape). Confirm validity via the oracle.

## The oracle (Julia is not installed by default)

Verification is weaker here than for a live-interpreter tool, so lean on layers:

- **JuliaSyntax differential oracle** — fatou's parser is measured against
  JuliaSyntax.jl (ported as a dev-dependency; no Julia install needed). Run it
  with `cargo test --test juliasyntax_oracle` (see
  `tests/juliasyntax_oracle.rs` and the `tests/fixtures/oracle/` corpus). This is
  the ground truth for parser outcomes on covered cases.
- **fatou's own losslessness** — `fatou parse --verify --quiet <file>` proves
  `reconstruct(text) == text`; `fatou parse --to sexp` prints the s-expression
  projection for comparing shape.
- **Live `julia`, if present** — `julia -e 'Meta.parse("...")'` is the direct
  validity check for a novel snippet. `command -v julia` first; if absent, say so
  and fall back to careful reasoning from Julia's grammar plus the differential
  oracle. Suggest the user install `julia` if a parser question needs a
  definitive answer.

A linter false positive is usually a *language-semantics* judgment (is this
legitimate Julia the linter wrongly flags?) and can often be settled from
knowledge of the language plus fatou's own parse tree, without a live
interpreter.

## Workflow

1. **Target.** Take the repo from the user's argument (a GitHub `owner/name`, a
   clone URL, or a local path). If none is given, propose a good default
   (`JuliaLang/julia` is huge and idiomatic; `JuliaData/DataFrames.jl` or
   `FluxML/Flux.jl` are smaller) and confirm before cloning.

2. **Setup (parallel/background).** Build the release binary and shallow-clone
   the target into the **session scratchpad directory** (not bare `/tmp`), at
   once:

   ```sh
   cargo build --release
   git clone --depth 1 https://github.com/<owner>/<name>.git "$SCRATCH/<name>"
   ```

3. **Lint the tree, capture everything.** Capture both streams (diagnostics may
   be on stderr; `lint` exits non-zero when it reports anything):

   ```sh
   target/release/fatou lint "$SCRATCH/<name>" >lint.out 2>lint.err
   ```

   If a non-UTF-8 or otherwise unreadable file aborts the run, move it aside and
   re-run (and note the abort as a robustness issue).

4. **Summarize by rule.** Count findings per rule to prioritize high-volume and
   high-risk buckets:

   ```sh
   grep -oE '(warning|error): [a-z-]+' lint.err | sort | uniq -c | sort -rn
   ```

   Scope- and macro-aware rules are the most false-positive-prone; the syntax/
   error bucket is where parser bugs hide (mind Julia's macros, `@`-forms,
   string macros, broadcasting, and `where`/parametric syntax).

5. **Triage (the heart of the work).** For each priority rule, pull real findings
   (`grep -B1 -A6 'warning: <rule>' lint.err`), open the cited source line, and
   **reduce each suspect to a minimal reproducer** piped to the tool:

   ```sh
   printf '...\n' | target/release/fatou lint
   printf '...\n' | target/release/fatou parse            # inspect the CST
   printf '...\n' | target/release/fatou parse --verify --quiet   # losslessness
   ```

   When a parse failure looks like a parser bug, **isolate the trigger by
   bisecting context** (which delimiter, comment vs no comment, which operator),
   varying one axis at a time until the minimal failing shape is pinned.

6. **Verify against the oracle.** Promote a suspicion to a bug only after the
   oracle agrees (JuliaSyntax test, live `julia` if present, or careful grammar
   reasoning noting the interpreter is absent).

7. **Fan out for volume (recommended).** For a big finding set, spawn parallel
   triage subagents—one per rule-bucket—each given the absolute
   `target/release/fatou` path, the `lint.err` path, the classification scheme,
   and the oracle recipe (including whether `julia` is installed). Each returns
   minimal reproducers, per-category verdicts, and an FP-rate assessment.

8. **Fix or record.** For the cleanest, well-isolated bugs, fix TDD-style,
   honoring fatou's tenets (parser bugs are fixed in the parser, never papered
   over downstream; losslessness is sacred):

   - Add a failing fixture first and **watch it fail**, following the
     conventions in fatou's `add-lint-rule` and parser-fixture setup (reduce the
     case from the corpus).
   - Fix at the root cause; re-verify against the oracle.
   - Run the gates: `cargo test`, `cargo clippy --all-targets --all-features --
     -D warnings`, `cargo fmt -- --check`; `cargo insta accept` after reviewing
     new snapshots.

   Record everything you don't fix as follow-ups in `TODO.md` in the house style,
   each with a minimal reproducer and the confirmed-correct Julia behavior.
   Commit only if the user asks—atomic, Conventional Commits.

9. **Report back.** State plainly: bugs found (fixed vs. documented) with
   copy-pasteable reproducers; false-positive categories per rule; incorrect-span
   issues; which rules you verified clean; and the follow-ups recorded. Be
   faithful about what was and wasn't oracle-verified—especially where `julia`
   was unavailable and you reasoned instead.

## fatou-specific notes

- **`fatou`'s parser is an architectural twin of arity's** (lossless rowan CST,
  salsa). Expect the same *classes* of parser bugs (trivia/continuation handling,
  bracket-context newline rules), adapted to Julia's grammar.
- **No live Julia oracle by default.** The JuliaSyntax differential test is the
  reliable ground truth for parser outcomes; ad-hoc snippet validation needs
  `julia` installed. Be explicit in the report when a verdict rests on reasoning
  rather than an interpreter.
- **Autofix correctness is correctness, not layout.** A fix must keep the code
  parseable and lossless, but need not respect line width (that's the
  formatter's job). Test a fix by applying it and re-parsing, never by reading it.
- **Julia FP hotspots:** macro hygiene and `@`-expansions, string/command
  macros, `do` blocks, broadcasting/`.`-fusion, keyword vs positional args, and
  `global`/`local`/`let` scope—these are where a scope- or call-aware rule is
  most likely to misfire.
