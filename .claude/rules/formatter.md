---
paths:
  - "crates/fatou-formatter/**/*.rs"
  - "crates/fatou-formatter/tests/fixtures/**/*"
  - "src/formatter.rs"
  - "src/formatter/**/*.rs"
  - "src/debug.rs"
---

# Formatter rules

Scope: formatter engine and CLI formatter bridge.

Engine: `crates/fatou-formatter/src/formatter/`. The root's `src/formatter.rs`
is a CLI bridge that re-exports it and hosts the batch `check` API.
**To grow the formatter, use the `formatter` skill**.

## Hard invariants

- **The formatter is the sole authority on layout** (Tenet 1). Output is decided
  solely by the rules and the layout engine, never by how the input happens to
  be written. **Push back on hard-coded special cases** for specific constructs.
- **No persistent line breaks.** Fatou **fully reflows**: it lays out each
  construct from scratch under `line_width` and breaks only where width or
  semantics require it, regardless of where the source broke. A rule that reads
  whether the author broke a line is a bug, not a feature. Likewise the input's
  whitespace, operator spelling (`in` vs `∈`, `a*b` vs `a * b`), and
  numeric-literal form never influence the result.
- **Idempotence:** `format(format(x)) == format(x)`, and the output must reparse
  cleanly. Byte-identical output is the bar for a "behavior-preserving" refactor.
- **Losslessness is assumed, not enforced here.** The CST is lossless; focus on
  layout. **Never paper over a parser bug in the formatter** (Tenet 3).
- **`wasm32-unknown-unknown`-clean**: no filesystem, process, thread, or clock
  use — a dprint plugin embeds the crate as a Wasm module and a CI job enforces
  it. Anything needing the filesystem belongs in the root-crate bridge.

Any deviation from full reflow is a deliberate, recorded choice, never silent
non-determinism.

## Engine shape

A Wadler/Prettier-style document IR (`ir.rs`) printed by a **single best-fit
layout engine** (`printer.rs`) that makes *all* line-break decisions. `core.rs`
exposes `format`/`format_with_style`; `style.rs` is `FormatStyle`.

`rules::lower` (`rules.rs`) walks the CST into IR. Constructs with a rule are
reshaped; **everything else is lowered transparently** (verbatim tokens, recurse
into children), so unhandled syntax stays byte-identical and the pass stays
idempotent while rules land incrementally. That transparent fallback is
load-bearing — do not replace it with whole-construct verbatim to get a shape
out.

The root-crate bridge (`src/formatter/check.rs`) hosts `check_paths`: file
walking and rayon stay out of the engine.

## Testing

Fatou owns its style; there is **no external reference formatter**.

- The gate is **hand-authored fixtures**:
  `crates/fatou-formatter/tests/fixtures/formatter/<slug>/` holds `input.jl` and
  a hand-written `expected.jl`, and `tests/formatter.rs` asserts
  `format(input.jl) == expected.jl`.
- **Presence of `expected.jl` is gate membership.** A fixture with only
  `input.jl` is a construct still being authored. There is no allowlist or
  blocked list.
- `expected.jl` is authored under Tenet 1, **never captured from any formatter**.
- `formatter_is_idempotent_and_stable` runs over **every** `input.jl` and checks
  `format(format(x)) == format(x)` plus clean reparse — including the
  not-yet-gated ones.
- Range formatting has a root-crate suite (`tests/range_format.rs`); CLI
  behavior is in `tests/format_cli.rs`.

## `fatou debug format` is a CI contract

`src/debug.rs` backs the per-file invariant checker that
`.github/workflows/smoke-test.yml` drives over real Julia repos. **Its output
strings are contracts**: the workflow greps the parenthesized failure labels
(`losslessness`, `idempotency`, `format-error`), parses the `Approx. diff start
line` bullet, and predicts dump-file names from `sanitize_path_for_filename`.
Change them in lockstep. Triage findings with the `smoke-test-triage` skill.

## Benchmarks: measured, never asserted

`task bench` (`bench/compare_format.sh`) times the formatter against Runic and
JuliaFormatter and writes the tracked `bench/results.json`, which is the sole
source of the published performance page. Never a quality gate; report ratios,
not milliseconds.
