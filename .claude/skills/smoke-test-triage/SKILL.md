---
name: smoke-test-triage
description: >-
  Triage and fix fatou smoke-test regressions (idempotency, losslessness,
  format-error, timeout) from CI debug-format reports and linked issues.
---

Use this skill when asked to investigate failures reported by the smoke-test
scan (`.github/workflows/smoke-test.yml`) or `debug format` CI issues,
especially idempotency and losslessness regressions.

## Goals

1. Reproduce the exact failure from the report.
2. Minimize to a stable local fixture.
3. Add regression coverage in the right test surface.
4. Fix root cause (not symptom).
5. Validate targeted cases, then the full repository checks.

## Triage workflow

1. Read the issue/report details first:
   - failing check type (`idempotency`, `losslessness`, `format-error`,
     `timeout`, or `unknown`)
   - sample file path
   - upstream repo + commit SHA
   - fatou commit/version used by the scan
   - report excerpt and the approximate diff start line

2. Reproduce in a local clone of the target repository:
   - the issue's `logs/…` paths (sample log, report, and the idempotency
     `input`/`once`/`twice` dumps) are relative to the run artifact; fetch it
     with `gh run download <run-id> -n debug-format-repo-scan-results` (the
     run id is in the issue's workflow-run link)
   - checkout the exact target commit from the report
   - run:
     - `fatou debug format --checks all --report <sample-file>`
   - if needed, collect pass artifacts with:
     - `fatou debug format --checks all --dump-dir <dir> --dump-passes <file>`
   - `fatou` here means the current checkout's binary — `cargo run
     --release --` or `target/release/fatou` — not an installed release
   - the scan runs with the target repo's own `fatou.toml` when it has one
     (falling back to `--no-config` only when that config is invalid), so
     reproduce under the same config.

3. Minimize:
   - reduce to the smallest snippet that still reproduces
   - keep the source realistic (string/command interpolation, macro calls
     with trailing keyword-like arguments, docstrings, operator and Unicode
     edges, and mixed line endings are common triggers)
   - confirm reproduction is deterministic across repeated runs

4. Classify the failure before fixing — **check the CST before any
   formatter-side fix**:
   - **Losslessness failure ⇒ always a parser bug** (tenet: losslessness is
     the parser's job, `reconstruct(text) == text` byte-for-byte). Fix in
     `crates/fatou-parser/src/parser/`, never by compensating in the formatter.
   - **Idempotency failure ⇒ find which pass diverges and why.** Use the
     `--dump-dir` artifacts to compare input vs `once` vs `twice`, then
     inspect `fatou parse` on the input and on the first-pass output. If
     the CST of the formatted output is *structurally* wrong — mis-attached
     arguments, a block parsed differently after reflow, trivia bound to the
     wrong node — **the bug is parser-side, no matter which pass shows the
     symptom**. Idempotency drift is a downstream symptom of upstream shape
     divergence. The JuliaSyntax differential oracle (`fatou parse --to
     sexpr`, `crates/fatou-parser/tests/juliasyntax_oracle.rs`, the `parser-parity` skill) is
     the structural reference for suspicious parses.
   - **Anti-pattern: fixing in the formatter because the symptom lives
     there.** If you find yourself reaching for a formatter helper to make
     pass1 == pass2 (normalizing whitespace, special-casing a node shape),
     stop and re-check the parse. A formatter fix is only correct when the
     CST is already right and the divergence is purely in rendering. The
     formatter deliberately owns full reflow (tenet 1: deterministic,
     canonical formatting — no persistent line breaks), so structural
     rewrites are allowed, but they must be meaning-preserving and reach a
     fixed point on the second pass.
   - **`format-error` ⇒ the input has parse diagnostics** (or, in the
     future, a construct the formatter refuses). Run `fatou parse <file>`
     and read the diagnostics: parity with JuliaSyntax is nearly complete,
     so treat this as a real signal — either the parser mis-parses valid
     Julia (fix the grammar or recovery; hand off to the `parser-parity`
     skill) or the file is genuinely broken upstream (record it, no fatou
     fix needed).
   - **`timeout` ⇒ a hang or pathological slowness.** Reproduce with a
     release build and narrow by bisecting the file;
     never-infinite-loop on unexpected input is a parser invariant.
   - **`unknown` ⇒ read the sample log before assuming a fatou bug.** The
     scan buckets any failure whose output matches no known label here —
     typically read or discovery errors (non-UTF-8 content, or a filename
     like a bare `.jl` dotfile that git's glob matches but file discovery
     rejects). Those are scan noise: fix or exclude them in the workflow and
     close the issue; there is nothing to fix in fatou.
   - If uncertain, state the best hypothesis and why before implementing —
     and include the relevant `fatou parse` output in the hypothesis.

5. Add regression fixture(s):
   - Parser bugs (losslessness, mis-parse): add an oracle fixture
     `crates/fatou-parser/tests/fixtures/oracle/<slug>/{input.jl, expected.sexpr}` and add the
     slug to `crates/fatou-parser/tests/oracle/allowlist.txt` (see the `parser-parity` skill;
     mint `expected.sexpr` with `scripts/update-juliasyntax-corpus.sh`,
     which needs the devenv Julia). Pure-losslessness bugs also get a case
     in the `lossless_corpus` test in `crates/fatou-parser/src/parser/core.rs`.
   - Formatter bugs (idempotency, layout): add a fixture
     `crates/fatou-formatter/tests/fixtures/formatter/<slug>/input.jl`. The
     `formatter_is_idempotent_and_stable` test in `crates/fatou-formatter/tests/formatter.rs` runs
     over **every** `input.jl`, gated or not, so this alone pins the
     invariant. Only add `expected.jl` through the `formatter` skill's
     human-in-the-loop flow — `expected.jl` is hand-authored by the user
     and its presence is gate membership; do not author it unilaterally.
   - `.gitattributes` already pins `*.jl` and `*.sexpr` to `eol=lf`; no
     per-fixture attributes work is needed.

6. Fix implementation at root cause:
   - parser lossless/CST bugs → `crates/fatou-parser/src/parser/`
   - formatting/idempotency bugs → `crates/fatou-formatter/src/formatter/`
   - avoid papering over by changing expected outputs only
   - preserve existing behavior for unrelated fixtures

7. Validate:
   - targeted first:
     - the new test (`cargo test -p fatou-formatter --test formatter` or
       `cargo test -p fatou-parser --test juliasyntax_oracle`)
     - `fatou debug format --checks all --report <fixture-or-sample-file>`
   - then full validation:
     - `cargo test --workspace`
     - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
     - `cargo fmt`
   - for parser/CST changes, regenerate the oracle report
     (`cargo test -p fatou-parser --test juliasyntax_oracle -- --ignored oracle_full_report`)
     and triage any new divergence per the `parser-parity` skill

## Fatou-specific guidance

- Formatting is a deterministic full reflow (tenet 1): semantically
  equivalent inputs must format identically, and source line breaks carry no
  meaning. Never "fix" idempotency by mirroring the input's layout.
- Comments and whitespace are trivia the parser preserves losslessly; a
  losslessness diff that drops or moves a comment is a CST attachment bug.
- Line endings resolve via `LineEnding::Auto` (CRLF input stays CRLF), so
  keep the original ending when minimizing a reproducer.
- Prefer one focused regression fixture per bug; do not update unrelated
  fixtures.

## Report-back format

When done, report:

1. Whether the issue reproduced (and the exact command).
2. Minimal reproducer summary.
3. Fixture(s) added/updated.
4. Root cause and code path changed.
5. Validation commands run and outcomes.
