---
name: parser-parity
description: >-
  Grow Fatou's Julia parser toward JuliaSyntax.jl using the differential oracle.
  The projector at crates/fatou-parser/src/parser/sexpr.rs walks the CST and
  emits JuliaSyntax's s-expression shape; the harness in
  crates/fatou-parser/tests/juliasyntax_oracle.rs diffs each fixture against
  pinned expected.sexpr. Use this skill to take a parser target — a divergence
  found by probing real Julia, a handover recorded in RECAP.md, or one named
  directly by an issue, a code snippet, or a prompt — then add the grammar plus
  projector support, lock it with a fixture, and ratchet the now-passing cases
  into the allowlists. The projector is a test-only diagnostic: a divergence
  means the CST (or the projector's encoding translation) is wrong, never patch
  it in the projector to make the test pass.
---

Use this skill when asked to advance Fatou's parser parity, chase a suspected
parser bug, or take a named construct. Read `TODO.md` and `RECAP.md` first for
roadmap priority, the latest session, queued parser targets, and traps.

## Where targets come from

The corpora are a **regression gate, not a queue**. Both are effectively
exhausted: the harvested corpus is 677/685 with the remaining 8 permanent (float
display, `(2)(3)x` juxtaposition, the `x 'y` char lexer), and the curated corpus
is fully allowlisted but for one blocked case. Regenerating the reports and
looking for something to do will come up empty. Real targets arrive three ways:

- **Probing real Julia.** The default when nothing else is queued. Take real
  code — a vendored tree, a cloned package, a `debug format` smoke-test failure
  — and diff Fatou's projection against the oracle in bulk (the recipe in step
  3). Most recent sessions found their target this way.
- **A handover in `RECAP.md`.** The "Queued parser targets" block at the top
  holds parser targets left by an earlier session or handed over from the
  formatter, linter, or another consumer. Prefer these over a cold probe.
- **Named directly.** An issue, a code snippet, a construct the user names, or a
  gap another skill hit and handed off mid-task. A user-named target always
  wins.

A target is worth taking when it is a **cluster** (one root cause unlocking
several shapes) or a **construct real code actually contains**. Both criteria
apply to error shapes exactly as they do to valid code — see the buckets below.

## Cross-skill handoff ownership

`TODO.md` is the authoritative roadmap and status record. Detailed active work
belongs in the receiving skill's `RECAP.md`: parser targets stay here, while a
formatter or linter follow-up unlocked by parser work goes to that consumer's
recap and `TODO.md` section. The sending recap may retain a historical session
note, but never a second active outbound queue. When work lands, the receiver
marks `TODO.md` and removes the item from its own active recap queue.

## The oracle in one paragraph

`parse(text)` → lossless rowan CST. `to_juliasyntax_sexpr(&cst)` projects it
into JuliaSyntax's `SyntaxNode` s-expression (e.g.
`(toplevel (= (call f x) …))`), translating only *encoding* differences (wrapper
nodes, delimiters, trivia) and leaving genuine *modeling* divergences faithful
so they surface. `normalize_sexpr` makes the diff whitespace-insensitive. Two
corpora, both pinned (no Julia at test time → CI-safe):

- **Curated dir corpus** `crates/fatou-parser/tests/fixtures/oracle/<slug>/`
  (`input.jl` + `expected.sexpr`), gated by `oracle_allowlist`; every case is
  accounted for in `allowlist.txt` **or** `blocked.txt` (the latter with a
  one-line rationale).
- **Harvested JuliaSyntax corpus**
  `crates/fatou-parser/tests/fixtures/oracle/juliasyntax.jsonl` (685 micro-cases
  extracted from JuliaSyntax's own `test/parser.jl` by
  `scripts/harvest-juliasyntax-corpus.jl`), gated **opt-in** by
  `oracle_juliasyntax` against `juliasyntax-allowlist.txt`.

Both pinned to the JuliaSyntax version in
`crates/fatou-parser/tests/fixtures/oracle/.juliasyntax-source`. Reports
(`crates/fatou-parser/tests/oracle/*report.txt`) are gitignored and regenerated
on demand.

## What this skill is NOT

- **Not a pass-rate chase.** A passing case can hide a wrong CST if the
  projector compensates. The projector must stay a faithful encoding
  translation; if a session's diff is mostly in `sexpr.rs` (beyond a genuine new
  node's mapping), that's a smell—the fix probably belongs in the parser.
- **Not "make Fatou's tree identical to Julia's."** Some divergences are
  deliberate Fatou modeling choices (comparison chains stay nested; associative
  `a*b*c` stays nested binary; numeric-literal display). Those are **recorded**
  (dir corpus → `blocked.txt`; JS corpus → just left out of the allowlist), not
  "fixed."
- **Not parser-only.** The formatter lowers the same CST, so a shape change can
  turn a formatter fixture red even when every parser test passes — see
  "Formatter coupling" below.

## Failure buckets (classify before fixing)

- **Projector gap**: Fatou parses fine, but the projector emits the wrong shape
  (missing node-kind arm, wrong head, encoding not unwrapped). Fix `sexpr.rs`.
- **Parser gap**: Fatou can't parse it (diagnostics → UNSUPPORTED) or parses it
  loosely (header passthrough as loose tokens). Fix `lexer.rs`/`expr.rs`. The
  main growth work.
- **Error shape**: Julia recovers and records the damage as `(error …)`/
  `(error-t …)`; Fatou's recovery differs in placement, nesting, or marker
  count. **In scope, and worked like any other bucket** — 110 of the 685
  harvested cases carry an error node, and the 2026-06-25 run landed a dozen of
  them. Ranked on the same two criteria as everything else (cluster size,
  real-world frequency), which in practice favors what half-typed code looks
  like — an unterminated ternary, a missing `end`, a stray sigil — over shapes
  you have to construct on purpose. Recovery is a **diagnostics side-channel**,
  never a tree node: record a `DiagnosticKind` at the right byte anchor and let
  the projector replay it.
- **Modeling divergence**: Fatou intentionally differs (associative flattening,
  comparison chains). Record, don't fix: dir → `blocked.txt` with rationale; JS
  → leave un-allowlisted.
- **Display normalization**: numeric literals (oct/bin→hex, `1_000`→`1000`,
  float canonicalization). Permanent divergence; blocked.

## Formatter coupling

`crates/fatou-formatter/src/formatter/rules.rs` lowers the same CST, matching on
node kinds and loose tokens. Changing a construct's **shape** — a wrapped node
becoming a loose token, a new node kind appearing, a child moving — can
therefore break a formatter fixture that the parser suite says nothing about.
Two rules:

- Run `cargo test --workspace`, not just `-p fatou-parser`, before believing a
  change is clean.
- When a formatter fixture goes red, the fix is to **move the rule to the new
  shape**, not to revert the parser. Worked example (2026-08-07c): making `∈` a
  loose for-spec separator moved the `∈` → `in` normalization from
  `lower_for_spec`'s wrapped-node arm to its flat one, which also made it cover
  the loop and `outer` forms for free. Check the fixture's `expected.jl` is
  still the desired output, and confirm idempotency on the new parser fixture.

## The operator recipe (4 files)

The token machinery is **table-driven**: a row in
`crates/fatou-parser/src/tokens.rs` expands into the `TokKind` variant, the
`SyntaxKind` variant, *and* the `syntax_kind_for` mapping, so a token cannot be
lexed but unmapped. Do not hand-add arms to `syntax.rs` or `tree_builder.rs` —
they are generated from that one row. (`ERROR` must stay the last `SyntaxKind`
variant; `kind_from_raw` uses it as the discriminant upper bound.)

Adding an infix/prefix operator touches exactly these, in order—miss one and it
won't lex, won't bind, or won't project:

1. `crates/fatou-parser/src/tokens.rs`: add the `token_table!` row (`TokKind`
   variant + `SyntaxKind` variant + mapping, all from that one line).
2. `crates/fatou-parser/src/parser/lexer.rs`: add an `OPS` row (`OPS`, \~line
   819) — the single table of fixed ASCII spellings, grouped by first byte and
        **longest-first within a group**; `build_op_index` const-asserts that
        ordering, which is what makes longest match a property of the data
        rather than of an arm. Unicode operators come from the generated
        code-point table (`unicode_op_at`) instead.
3. `crates/fatou-parser/src/parser/expr.rs`: add to `infix_binding_power`
   (\~line 3879; probe Julia for the tier and associativity first; right-assoc
   has `r_bp < l_bp`). Default operators build a `BINARY_EXPR`; assignment-like
   ones need a node-kind arm too.
4. `crates/fatou-parser/src/parser/sexpr.rs`: add to `infix_head`
   (`CallI`/`Special`/`DotCallI`/ `Dot`) **and** `is_operator`.

Keywords work the same way one level up: a `keywords.rs` row feeds
`keyword_table!`, whose rows are spliced into `token_table!`, so one row gets
the keyword its two enum variants, its mapping, and every keyword predicate.

Non-operator features (markers, quotes, literals) are usually just
`parse_prefix`

+ a `SyntaxKind` + a projector arm. `BEGIN_MARKER` (`0e0fc0e`) and `QUOTE_SYM`
  (`7199814`) are the worked examples; `parse_quote_sym` mirrors
  `parse_interpolation`.

## Workflow (per session)

1. **Read `TODO.md` and `RECAP.md`** (roadmap priority, traps, latest session,
   queued parser targets). A user-named target wins; otherwise follow the
   Parser section of `TODO.md`, then the recap queue, then probe (step 3).

2. **Baseline**: `cargo test --workspace`—note it's green. "No regression" =
   still green at the end.

3. **Find a target** (skip if you already have one). Probe real Julia in bulk:
   point Fatou and the oracle at the same files and diff. For a corpus of whole
   files, loop; for a set of snippets, split one delimited file:
   ```sh
   # per file: byte-identical or a divergence to triage
   diff <(julia --startup-file=no -e 'using JuliaSyntax;
            print(JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, read(ARGS[1], String);
                                       ignore_errors=true))' "$f") \
        <(cargo run -q -- parse --to sexpr "$f")
   ```
   Sources that have paid off: a vendored/cloned real package, the files behind
   a `debug format` smoke-test issue, and the sibling constructs of whatever the
   last session touched. Triage the diffs into buckets and pick a **cluster**
   (one root cause, several shapes) or a construct real code contains. The
   corpus reports are a *regression gate*, not a source of targets, but
   regenerate them if you want the current picture:
   ```sh
   cargo test -p fatou-parser --test juliasyntax_oracle -- --ignored
   ```
   (`crates/fatou-parser/tests/oracle/{report,juliasyntax-report}.txt`; FAIL =
   divergence, UNSUPPORTED = Fatou can't parse it.)

4. **A/B every candidate divergence before calling it a bug.** Stash the working
   tree and re-run the same input, so you know whether a diff is new or
   pre-existing — a pre-existing error shape is not a regression you introduced,
   and mistaking one for a regression burns the session.

5. **Probe Julia for the exact target shape**:
   ```sh
   julia --startup-file=no -e 'using JuliaSyntax;
     print(JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, raw"""<CODE>"""; ignore_errors=true))'
   ```
   **Trap:** inputs containing `"` or `$` break shell `raw"""…"""`—write the
   snippets to a temp file and loop `eachline` instead. Probe
   precedence/associativity explicitly (`a OP b OP c`, `a OP b ⊕ c`, both
   orders) before choosing a binding-power tier.

6. **Classify** into a bucket, then apply the **smallest** fix. Inspect the
   current CST shape via `cargo run -q -- parse <file>` and the projection via
   `cargo run -q -- parse --to sexpr <file>`.

7. **TDD fixture**—add
   `crates/fatou-parser/tests/fixtures/parser/<name>/input.jl` (valid cases that
   should match Julia; keep deferred edge cases out so it can be allowlisted),
   verify losslessness (`cargo run -q -- parse --verify --quiet <file>`), then
   review and accept the snapshot:
   ```sh
   cargo test -p fatou-parser --test parser_snapshots        # writes .snap.new
   cargo insta review                         # or: cargo insta accept
   ```
   **Read the CST before accepting**—confirm the shape is what you intend.

8. **Wire into the oracle dir corpus**:
   ```sh
   mkdir -p crates/fatou-parser/tests/fixtures/oracle/<name>
   cp crates/fatou-parser/tests/fixtures/parser/<name>/input.jl crates/fatou-parser/tests/fixtures/oracle/<name>/input.jl
   bash scripts/update-juliasyntax-corpus.sh   # mints expected.sexpr (needs devenv julia)
   diff <(cargo run -q -- parse --to sexpr crates/fatou-parser/tests/fixtures/oracle/<name>/input.jl) \
        crates/fatou-parser/tests/fixtures/oracle/<name>/expected.sexpr   # expect identical
   ```

9. **Re-triage + reseed allowlists** (both corpora). Regenerate reports, then
   keep each allowlist's comment header and replace its slug list with the
   current PASS set (header-length-agnostic):
   ```sh
   cargo test -p fatou-parser --test juliasyntax_oracle -- --ignored
   { grep -E '^#|^$' crates/fatou-parser/tests/oracle/allowlist.txt; \
     grep '^PASS' crates/fatou-parser/tests/oracle/report.txt | awk '{print $2}' | sort; } \
     > /tmp/al && mv /tmp/al crates/fatou-parser/tests/oracle/allowlist.txt
   { grep -E '^#|^$' crates/fatou-parser/tests/oracle/juliasyntax-allowlist.txt; \
     grep '^PASS' crates/fatou-parser/tests/oracle/juliasyntax-report.txt | awk '{print $2}' | sort; } \
     > /tmp/jal && mv /tmp/jal crates/fatou-parser/tests/oracle/juliasyntax-allowlist.txt
   ```
   Confirm the pass count went **up** (or held) and divergence didn't rise —
   that's the regression check. For a genuine new divergence in the curated dir
   corpus, add it to `blocked.txt` with a rationale instead.

10. **Guardrails**:
    ```sh
    cargo test --workspace
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo fmt --all -- --check
    ```
    `--workspace`, not `-p fatou-parser`: a shape change can break a formatter
    fixture (see "Formatter coupling"). Also format the new parser fixture and
    re-format the output, confirming idempotency.

11. Update `TODO.md` and the affected recaps. Mark the parser target's status,
    add the new parser session, demote the previous "Latest session" heading to
    "Earlier session", and update the Progress counts. If the parser change
    leaves active work for another component, record it in that receiving
    skill's recap and `TODO.md` section, not in this recap's queue. Keep this
    file to its stated cap by collapsing the oldest full section into a
    one-liner in "Earlier sessions".

12. **Commit.** Conventional Commits; subject ≤ 60 chars. New parsing
    capability/public API = `feat(parser)`; test-infra-only = `test(parser)`.
    The pre-commit hook runs clippy + rustfmt—never `--no-verify`. Don't push
    unless asked.

## Session boundaries

A committed target with `RECAP.md` updated (the end of step 12) **is a clean
stop**—`RECAP.md` is the handoff, so nothing valuable lives only in the chat
context. When the user asks to keep going, recommend by where you are:

- **Fresh session**—default at a committed boundary. The next session re-reads
  `RECAP.md` (step 1) and continues; a fresh, lean context keeps attention on
  the new target (exploration dumps, snapshot reviews, and triage reports from
  the finished target are pure ballast for the next one).
- **Compact & continue**—when the user wants the *next* target immediately and
  doesn't want to re-establish the green baseline. RECAP still protects the
  work.
- **Continue as-is**—only mid-target: uncommitted work, a half-applied fix, or a
  failing test you're chasing. Don't span more than one target in a context.

So: one target per context window is the intended cadence—the rolling log exists
precisely so you don't have to.

## Key files

- `crates/fatou-parser/src/parser/sexpr.rs`: projector (`to_juliasyntax_sexpr`,
  `normalize_sexpr`, `infix_head`, `is_operator`, per-kind `project_*`). The
  faithful diagnostic.
- `crates/fatou-parser/src/parser/expr.rs`: Pratt parser: `parse_prefix`,
  `infix_binding_power` (the precedence table \~line 3879), `ExprFlags`
  (threaded context like `end_marker`/`begin_marker`), the operator loop.
- `crates/fatou-parser/src/parser/lexer.rs`: tokenization; the `OPS` table
  (\~line 819) and `build_op_index`.
- `crates/fatou-parser/src/tokens.rs` (+ `keywords.rs`): `token_table!` /
  `keyword_table!` — one row generates the `TokKind` variant, the `SyntaxKind`
  variant, and the mapping. The growth surface for a new token.
- `crates/fatou-parser/src/syntax.rs`: `SyntaxKind`, generated from
  `token_table!` (`ERROR` must stay last).
- `crates/fatou-parser/src/parser/tree_builder.rs`: `syntax_kind_for`, also
  generated from `token_table!`.
- `crates/fatou-parser/src/parser/diagnostics.rs`: `DiagnosticKind` — the
  recovery side-channel the projector replays as `(error …)`/`(error-t …)`.
- `crates/fatou-formatter/src/formatter/rules.rs`: consumes the same CST; the
  thing a shape change breaks (see "Formatter coupling").
- `crates/fatou-parser/tests/juliasyntax_oracle.rs`: harness (allowlist gates +
  ignored reports).
- `scripts/update-juliasyntax-corpus.{sh,jl}`: regen pinned `expected.sexpr`.
- `scripts/harvest-juliasyntax-corpus.jl`: re-extract the JS corpus (run on a
  JuliaSyntax version bump, then re-triage).

## Traps

- **Reseeding must preserve the allowlist header.** Use the `grep -E '^#|^$'`
  recipe above; don't clobber the comment block.
- **Reports are gitignored and untracked.**
  `crates/fatou-parser/tests/oracle/{report,juliasyntax-report}.txt` regenerate
  from the ignored tests; never commit them (they were force-added once and
  untracked again in 2026-08-07c), never hand-edit `expected.sexpr` (regenerate
  via the refresh script).
- **The corpora do not hold the frontier.** Both are exhausted of fixable cases;
  a green report means "no regression", not "nothing to do". Targets come from
  probing real code, a RECAP handover, or a direct ask.
- **Shell `raw"""…"""` probes break on `"`/`$`.** Use a temp file.
- **Whitespace-sensitive disambiguation.** Julia distinguishes `a[begin]`
  (marker) from `[begin x end]` (block), `:foo` (quote) from `a[:]` (Colon),
  `A'` (transpose) from `A '` (char), `[1 +2]` from `[1 + 2]`. Probe both forms
  before scoping a feature, or you'll regress the sibling.
- **The harvested corpus is opt-in; the curated one is opt-out.** A new
  divergence in the JS corpus just stays un-allowlisted (visible in the report);
  a new divergence in the dir corpus must go to `blocked.txt` or the gate goes
  red.
- **Version pin.** The corpus is pinned to one JuliaSyntax version. A bump means
  re-running both `scripts/*.jl`, re-triaging, and updating
  `.juliasyntax-source`.

## Report-back format

1. Construct landed (e.g. "pair operator `=>`").
2. JS corpus: pass/divergence/unsupported before → after (+ regressions: must be
   zero).
3. Dir allowlist + JS allowlist counts before → after.
4. Files changed, by failure bucket.
5. New fixtures + new blocked entries (with rationale).
6. Ranked next target. If ending uncommitted/with regressions, say so explicitly
   and list the red tests.
