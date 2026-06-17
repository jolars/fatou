---
name: parser-parity
description: >-
  Grow Fatou's Julia parser toward JuliaSyntax.jl using the differential oracle.
  The projector at src/parser/sexpr.rs walks the CST and emits JuliaSyntax's
  s-expression shape; the harness in tests/juliasyntax_oracle.rs diffs each
  fixture against pinned expected.sexpr. Use this skill to pick the next gap from
  the corpus, add the grammar plus projector support, lock it with a fixture, and
  ratchet the now-passing cases into the allowlists. The projector is a test-only
  diagnostic: a divergence means the CST (or the projector's encoding translation)
  is wrong, never patch it in the projector to make the test pass.
---

Use this skill when asked to advance Fatou's parser parity, work the
JuliaSyntax-oracle backlog, or "take the next gap." Read `RECAP.md` first for
the latest session, ranked next targets, and traps.

## The oracle in one paragraph

`parse(text)` → lossless rowan CST. `to_juliasyntax_sexpr(&cst)` projects it
into JuliaSyntax's `SyntaxNode` s-expression (e.g. `(toplevel (= (call f x) …))`),
translating only *encoding* differences (wrapper nodes, delimiters, trivia) and
leaving genuine *modeling* divergences faithful so they surface. `normalize_sexpr`
makes the diff whitespace-insensitive. Two corpora, both pinned (no Julia at test
time → CI-safe):

- **Curated dir corpus** `tests/fixtures/oracle/<slug>/` (`input.jl` +
  `expected.sexpr`), gated by `oracle_allowlist`; every case is accounted for in
  `allowlist.txt` **or** `blocked.txt` (the latter with a one-line rationale).
- **Harvested JuliaSyntax corpus** `tests/fixtures/oracle/juliasyntax.jsonl`
  (~575 micro-cases extracted from JuliaSyntax's own `test/parser.jl` by
  `scripts/harvest-juliasyntax-corpus.jl`), gated **opt-in** by
  `oracle_juliasyntax` against `juliasyntax-allowlist.txt`. The full report's
  divergence + unsupported buckets are the backlog.

Both pinned to the JuliaSyntax version in
`tests/fixtures/oracle/.juliasyntax-source`. Reports
(`tests/oracle/*report.txt`) are gitignored and regenerated.

## What this skill is NOT

- **Not a pass-rate chase.** A passing case can hide a wrong CST if the projector
  compensates. The projector must stay a faithful encoding translation; if a
  session's diff is mostly in `sexpr.rs` (beyond a genuine new node's mapping),
  that's a smell — the fix probably belongs in the parser.
- **Not "make Fatou's tree identical to Julia's."** Some divergences are
  deliberate Fatou modeling choices (comparison chains stay nested; associative
  `a*b*c` stays nested binary; numeric-literal display). Those are **recorded**
  (dir corpus → `blocked.txt`; JS corpus → just left out of the allowlist), not
  "fixed."
- **Not error-shape work.** Cases where Julia emits `(error …)` recovery are
  skipped at harvest and deferred; the harness skips dir cases that produce Fatou
  diagnostics. Error-shape parity is a separate future phase.

## Failure buckets (classify before fixing)

- **Projector gap** — Fatou parses fine, but the projector emits the wrong shape
  (missing node-kind arm, wrong head, encoding not unwrapped). Fix `sexpr.rs`.
- **Parser gap** — Fatou can't parse it (diagnostics → UNSUPPORTED) or parses it
  loosely (header passthrough as loose tokens). Fix `lexer.rs`/`expr.rs`. This is
  the bulk of the backlog and the main growth work.
- **Modeling divergence** — Fatou intentionally differs (associative flattening,
  comparison chains). Record, don't fix: dir → `blocked.txt` with rationale; JS →
  leave un-allowlisted.
- **Display normalization** — numeric literals (oct/bin→hex, `1_000`→`1000`,
  float canonicalization). Permanent divergence; blocked.
- **Error-shape** — recovery nodes. Deferred.

## The operator recipe (5 files)

Adding an infix/prefix operator touches exactly these, in order — miss one and it
won't lex, won't get a kind, or won't project:

1. `src/parser/lexer.rs` — add a `TokKind` variant + lex it (longest-match: the
   3-char dotted table, then the 2-char table, then 1-char). `=>`/`.=>` is the
   worked example (commit `c6448f2`).
2. `src/syntax.rs` — add the `SyntaxKind` operator token (keep `ERROR` last).
3. `src/parser/tree_builder.rs` — map `TokKind::X => SyntaxKind::X`.
4. `src/parser/expr.rs` — add to `infix_binding_power` (probe Julia for the tier
   and associativity first; right-assoc has `r_bp < l_bp`). Default operators
   build a `BINARY_EXPR`; assignment-like ones need a node-kind arm too.
5. `src/parser/sexpr.rs` — add to `infix_head` (`CallI`/`Special`/`DotCallI`/
   `Dot`) **and** `is_operator`.

Non-operator features (markers, quotes, literals) are usually just `parse_prefix`
+ a `SyntaxKind` + a projector arm. `BEGIN_MARKER` (`0e0fc0e`) and `QUOTE_SYM`
(`7199814`) are the worked examples; `parse_quote_sym` mirrors `parse_interpolation`.

## Workflow (per session)

1. **Read `RECAP.md`** (traps, latest session, ranked next targets). Prefer a
   user-named target.
2. **Baseline**: `cargo test` — note it's green. "No regression" = still green at
   the end.
3. **Regenerate the report**:
   ```sh
   cargo test --test juliasyntax_oracle -- --ignored juliasyntax_full_report
   ```
   Read `tests/oracle/juliasyntax-report.txt` (FAIL = divergence, UNSUPPORTED =
   Fatou can't parse it = the frontier). `-- --ignored` alone runs both reports.
4. **Pick a target**: a cluster of FAIL/UNSUPPORTED sharing one likely root cause
   (one fix unlocks several), or a small, high-real-world-value construct.
5. **Probe Julia for the exact target shape**:
   ```sh
   julia --startup-file=no -e 'using JuliaSyntax;
     print(JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, raw"""<CODE>"""; ignore_errors=true))'
   ```
   **Trap:** inputs containing `"` or `$` break shell `raw"""…"""` — write the
   snippets to a temp file and loop `eachline` instead. Probe precedence /
   associativity explicitly (`a OP b OP c`, `a OP b ⊕ c`, both orders) before
   choosing a binding-power tier.
6. **Classify** into a bucket, then apply the **smallest** fix. Inspect the
   current CST shape via `cargo run -q -- parse <file>` and the projection via
   `cargo run -q -- parse --to sexpr <file>`.
7. **TDD fixture** — add `tests/fixtures/parser/<name>/input.jl` (valid cases that
   should match Julia; keep deferred edge cases out so it can be allowlisted),
   verify losslessness (`cargo run -q -- parse --verify --quiet <file>`), then
   review and accept the snapshot:
   ```sh
   cargo test --test parser_snapshots        # writes .snap.new
   cargo insta review                         # or: cargo insta accept
   ```
   **Read the CST before accepting** — confirm the shape is what you intend.
8. **Wire into the oracle dir corpus**:
   ```sh
   mkdir -p tests/fixtures/oracle/<name>
   cp tests/fixtures/parser/<name>/input.jl tests/fixtures/oracle/<name>/input.jl
   bash scripts/update-juliasyntax-corpus.sh   # mints expected.sexpr (needs devenv julia)
   diff <(cargo run -q -- parse --to sexpr tests/fixtures/oracle/<name>/input.jl) \
        tests/fixtures/oracle/<name>/expected.sexpr   # expect identical
   ```
9. **Re-triage + reseed allowlists** (both corpora). Regenerate reports, then keep
   each allowlist's comment header and replace its slug list with the current PASS
   set (header-length-agnostic):
   ```sh
   cargo test --test juliasyntax_oracle -- --ignored
   { grep -E '^#|^$' tests/oracle/allowlist.txt; \
     grep '^PASS' tests/oracle/report.txt | awk '{print $2}' | sort; } \
     > /tmp/al && mv /tmp/al tests/oracle/allowlist.txt
   { grep -E '^#|^$' tests/oracle/juliasyntax-allowlist.txt; \
     grep '^PASS' tests/oracle/juliasyntax-report.txt | awk '{print $2}' | sort; } \
     > /tmp/jal && mv /tmp/jal tests/oracle/juliasyntax-allowlist.txt
   ```
   Confirm the pass count went **up** (or held) and divergence didn't rise —
   that's the regression check. For a genuine new divergence in the curated dir
   corpus, add it to `blocked.txt` with a rationale instead.
10. **Guardrails**:
    ```sh
    cargo test
    cargo clippy --all-targets --all-features -- -D warnings
    cargo fmt -- --check
    ```
11. **Update `TODO.md`** (mark a grammar bullet `[x]` mirroring the existing
    style; trim the construct from the oracle backlog list) and **`RECAP.md`**.
12. **Commit.** Conventional Commits; subject ≤ 60 chars. New parsing capability /
    public API = `feat(parser)`; test-infra-only = `test(parser)`. The pre-commit
    hook runs clippy + rustfmt — never `--no-verify`. Don't push unless asked.

## Session boundaries

A committed target with `RECAP.md` updated (the end of step 12) **is a clean
stop** — `RECAP.md` is the handoff, so nothing valuable lives only in the chat
context. When the user asks to keep going, recommend by where you are:

- **Fresh session** — default at a committed boundary. The next session re-reads
  `RECAP.md` (step 1) and continues; a fresh, lean context keeps attention on the
  new target (exploration dumps, snapshot reviews, and triage reports from the
  finished target are pure ballast for the next one).
- **Compact & continue** — when the user wants the *next* target immediately and
  doesn't want to re-establish the green baseline. RECAP still protects the work.
- **Continue as-is** — only mid-target: uncommitted work, a half-applied fix, or a
  failing test you're chasing. Don't span more than one target in a context.

So: one target per context window is the intended cadence — the rolling log exists
precisely so you don't have to.

## Key files

- `src/parser/sexpr.rs` — projector (`to_juliasyntax_sexpr`, `normalize_sexpr`,
  `infix_head`, `is_operator`, per-kind `project_*`). The faithful diagnostic.
- `src/parser/expr.rs` — Pratt parser: `parse_prefix`, `infix_binding_power`
  (the precedence table ~line 1525), `ExprFlags` (threaded context like
  `end_marker`/`begin_marker`), the operator loop.
- `src/parser/lexer.rs` — `TokKind` + tokenization (operator tables ~line 757).
- `src/syntax.rs` — `SyntaxKind` (`ERROR` must stay last).
- `src/parser/tree_builder.rs` — `TokKind` → `SyntaxKind` mapping.
- `tests/juliasyntax_oracle.rs` — harness (allowlist gates + ignored reports).
- `scripts/update-juliasyntax-corpus.{sh,jl}` — regen pinned `expected.sexpr`.
- `scripts/harvest-juliasyntax-corpus.jl` — re-extract the JS corpus (run on a
  JuliaSyntax version bump, then re-triage).

## Traps

- **Reseeding must preserve the allowlist header.** Use the `grep -E '^#|^$'`
  recipe above; don't clobber the comment block.
- **Reports are gitignored.** `tests/oracle/{report,juliasyntax-report}.txt`
  regenerate from the ignored tests; never commit them, never hand-edit
  `expected.sexpr` (regenerate via the refresh script).
- **Shell `raw"""…"""` probes break on `"`/`$`.** Use a temp file.
- **Whitespace-sensitive disambiguation.** Julia distinguishes `a[begin]` (marker)
  from `[begin x end]` (block), `:foo` (quote) from `a[:]` (Colon), `A'`
  (transpose) from `A '` (char), `[1 +2]` from `[1 + 2]`. Probe both forms before
  scoping a feature, or you'll regress the sibling.
- **The harvested corpus is opt-in; the curated one is opt-out.** A new divergence
  in the JS corpus just stays un-allowlisted (visible in the report); a new
  divergence in the dir corpus must go to `blocked.txt` or the gate goes red.
- **Version pin.** The corpus is pinned to one JuliaSyntax version. A bump means
  re-running both `scripts/*.jl`, re-triaging, and updating `.juliasyntax-source`.

## Report-back format

1. Construct landed (e.g. "pair operator `=>`").
2. JS corpus: pass / divergence / unsupported before → after (+ regressions: must
   be zero).
3. Dir allowlist + JS allowlist counts before → after.
4. Files changed, by failure bucket.
5. New fixtures + new blocked entries (with rationale).
6. Ranked next target. If ending uncommitted/with regressions, say so explicitly
   and list the red tests.
