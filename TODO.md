# TODOs

## Parser

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do). Deferred: the whole wrapping-operator family
  is unreleased in JuliaSyntax — the latest release (1.0.2) still rejects every
  form (`lex_plus`/`lex_minus`/`lex_star` have no `%`-suffix handling, verified
  2026-08-03), so no oracle bump can pin these until JuliaSyntax ships them.
  Implementing the lexer change now would be validatable only against a
  hand-authored parser fixture, not the differential oracle.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`src/parser/structural.rs`). Junk after a complete labeled keyword drops
  JuliaSyntax's trailing zero-width marker (`break l x y` ⇒ `(break l x)
  (error-t y)`, not `(error-t y (error-t))`), and a bare comma after one does not
  fold into a tuple (`break l, y` ⇒ `(break l) (error-t ✘ y)`, not
  `(tuple (break l) y)`) — the latter predates labels, since `break, y` behaved
  the same way. Both are error/edge shapes no corpus code hits, and neither is
  pinnable: labeled `break`/`continue` is unreleased in JuliaSyntax (the latest
  release, 1.0.2, still rejects `break lbl`, verified 2026-08-03), so no oracle
  bump can pin these until JuliaSyntax ships the feature.

### Incremental

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

- [x] Canonicalize the gap before a macro's *attached* argument
  (`lower_macro_call` in `src/formatter/rules.rs`). A gap before a bare
  `{…}`/`[…]` opener that is the sole argument is now dropped and glued to the
  idiomatic form (`@m {a}` -> `@m{a}`), since the two are the same program. This
  needs no parent context: a `[…]`/`(…)` suffix would have folded into the child
  (`@m {a}[x]` parses the arg as a compound `INDEX_EXPR`), so a bare sole opener
  never carries one. Parens/tuples stay excluded (`@foo(a, b)` != `@foo (a, b)`).

- [x] Extend the where-bound break-priority to *short multi-parameter* bounds.
  A short multi-param bound over a call signature (`f(longargs...) where {T, S}`)
  now breaks the args and keeps the bound flat on the closing line, via the new
  `Ir::CondGroup` conditional-layout primitive: `primary` keeps the bound flat,
  `fallback` explodes it, and the printer picks by measuring whether the flat
  bound fits the re-indented closing line `) where {…}`. A genuinely wide bound
  (`where_break`) still explodes. Fixture `where_short_multiparam_break`.

- [x] Extend the short multi-param where-priority to *return-type* signatures
  (`g(x)::T where {T, S}`). The `where`-lhs is a `TYPE_ANNOTATION`, so the
  `CondGroup` probe now uses the annotated closing-line prefix `)::T where ` (the
  `::T` flattened via `render_flat`), not `) where `. The new `where_closing_prefix`
  helper computes it for both a bare call and an annotated call; any other lhs
  keeps the plain exploding bound. Fixture `where_return_type_multiparam_break`.

## Linter

### Rules

- [ ] `index-from-length` (suspicious, syn, opinionated): `for i in
  1:length(x)` where `i` indexes `x` -> suggest `eachindex`/`axes`; also
  iterating a bare numeric literal (`for i in 3.5`). Name-based match on
  `length`/`size` (no resolution); StaticLint exempts known `Vector`/`Array`
  bindings, which we cannot without type info -- document as opinionated.
  (IncorrectIterSpec, IndexFromLength)

## Language server

- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.
- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.

## Tooling

- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
