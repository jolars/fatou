# TODOs

## Parser

- [ ] NFC-normalize identifiers in the s-expr projector (`src/parser/sexpr.rs`)
  to match JuliaSyntax, which applies `normalize_identifier` (NFC) so `y` +
  U+0302 prints as precomposed `ŷ`. The CST keeps raw source bytes (losslessness
  requires it), so this belongs in the projector's encoding translation, not the
  parser. Until then, oracle fixtures must use NFC-stable identifiers (see
  `tests/fixtures/oracle/unicode_identifiers`).

- [ ] Accept parenthesized `$(...)` interpolation as an import-path component
  (`src/parser/expr.rs`). The import-path parser takes a bare `$name` but
  rejects `$(name)`, so `import a.$(b)` and relative forms like
  `:(import ..($(x)).$(y))` raise `trailing tokens after statement` where
  JuliaSyntax parses `(import (importpath a ($ b)))`. Surfaced by the
  `fonsp/Pluto.jl` smoke-test scan (`deleting globals.jl`, a `format-error`,
  distinct from the losslessness fix in #39).

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do), so this is deferred until the pinned oracle
  advances past JuliaSyntax 0.4.10, which predates the whole family.

- [ ] Fuse the broadcast Unicode *radical* `.√` (and `.∛`, `.∜`, `.¬`) in
  `src/parser/lexer.rs`, and project it `dotcall-pre`. The infix Unicode tiers
  already fuse (`.×`, `.⊕`), but the prefix-only radicals fall through to a lone
  `.` plus the radical, so `.√[3,4,5]` parses as `(call-pre √ …)` where
  JuliaSyntax has `(dotcall-pre √ …)`. Surfaced by the `JuliaLang/julia` scan
  (`test/broadcast.jl`, a `format-error`).

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

- [ ] Prefer breaking a function signature's argument list over exploding a
  short `where` clause. Today an over-width `f(a, b, c) where {T}` breaks the
  single-param where bound (`where {\n    T,\n}`) rather than the args, because
  the where bound is a breakable group (`lower_where` in
  `src/formatter/rules.rs`). This is idempotent and matches the `where_break`
  fixture's break-when-long convention, but the args-broken form is more
  idiomatic when the bound is short. Needs a hand-authored `expected.jl` via the
  formatter flow.

## Linter

### Rules

- [ ] `index-from-length` (suspicious, syn, opinionated): `for i in
  1:length(x)` where `i` indexes `x` -> suggest `eachindex`/`axes`; also
  iterating a bare numeric literal (`for i in 3.5`). Name-based match on
  `length`/`size` (no resolution); StaticLint exempts known `Vector`/`Array`
  bindings, which we cannot without type info -- document as opinionated.
  (IncorrectIterSpec, IndexFromLength)
- [ ] `type-piracy` (correctness): extending an imported function with no
  owned argument type. Blocked on cross-file import and ownership
  resolution. (TypePiracy)

## Language server

- [ ] On-disk cache keyed by (name, version or `git-tree-sha1`), harvested in
  parallel (rayon) on the index pool, hot-swapped into the HIGH-durability
  `LibraryIndex` salsa input (the input itself has landed: a singleton in
  `src/incremental.rs` holding `BTreeMap<String, Arc<PackageIndex>>`, with
  `set_library_packages`/`set_package_index`/`library_package` on the db and
  `tests/library_index.rs`); re-analyze open files on swap.
- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.
- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.

## Tooling

- [ ] `build.rs` generating shell completions + man pages
  (clap_complete/clap_mangen), as arity does.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
