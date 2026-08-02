# TODOs

## Parser

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do), so this is deferred until the pinned oracle
  advances past JuliaSyntax 0.4.10, which predates the whole family.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`src/parser/structural.rs`). Junk after a complete labeled keyword drops
  JuliaSyntax's trailing zero-width marker (`break l x y` ⇒ `(break l x)
  (error-t y)`, not `(error-t y (error-t))`), and a bare comma after one does not
  fold into a tuple (`break l, y` ⇒ `(break l) (error-t ✘ y)`, not
  `(tuple (break l) y)`) — the latter predates labels, since `break, y` behaved
  the same way. Both are error/edge shapes no corpus code hits, and neither is
  pinnable until the oracle advances past JuliaSyntax 0.4.10, which predates the
  whole feature.

### Incremental

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

- [ ] Canonicalize the gap before a macro's *attached* argument
  (`lower_macro_call` in `src/formatter/rules.rs`). The gap is preserved
  verbatim because it is meaning-bearing whenever a `[…]`/`(…)` suffix follows
  (`@NamedTuple{T}[x]` indexes the type; `@NamedTuple {T}[x]` hands the macro
  `{T}[x]`), so `@m{a}` and `@m {a}` both survive even where no suffix makes
  them differ. Deciding "no suffix follows" needs the parent context, which
  `lower_macro_call` does not have today.

- [ ] Extend the where-bound break-priority to *short multi-parameter* bounds.
  A short but multi-param bound with long args (`f(longargs...) where {T, S}`)
  still explodes the bound rather than the args, because the element-count
  heuristic in `lower_where` only treats a single param as atomic. "Short"
  (fits on the closing line) is a width judgment the heuristic cannot make;
  handling it needs a conditional-layout primitive (like `HugGroup`) that
  chooses args-break-with-flat-bound over flat-args-with-broken-bound by
  measuring which first line fits.

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
