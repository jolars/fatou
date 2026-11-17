# fatou-parser

A lossless CST parser, typed AST wrappers, and incremental reparser for the
Julia language, built on [rowan](https://crates.io/crates/rowan).

This is the parsing engine of [fatou](https://fatou.dev), extracted into its own
crate so other tools can embed it. The tree preserves every byte of the input
(including whitespace and comments), parse errors are recoverable diagnostics
rather than failures, and edited buffers can be reparsed incrementally. The
crate builds for `wasm32-unknown-unknown`: it uses no filesystem, process,
thread, or clock facilities.

## Usage

```rust
use fatou_parser::parser::parse;

let output = parse("f(x) = x + 1\n");
assert!(output.diagnostics.is_empty());
assert_eq!(output.cst.text().to_string(), "f(x) = x + 1\n");
```

Wrapping arithmetic includes broadcast `.+%`, `.-%`, and `.*%`, along with their
`.+%=`, `.-%=`, and `.*%=` assignment forms. Each has a distinct `SyntaxKind`
token and follows its undotted counterpart's precedence.

Space-form macro arguments stop before operators on following lines, even inside
parentheses: `(@m x\n - y)` parses as `(@m x) - y`. The boundary holds across
comments and CRLF. The special two-argument `@doc` continuation retains its own
newline rule.

`break label, y` forms an enclosing tuple, while `break label x, y` gives the
labeled break a tuple value. `continue` accepts a label without a value.
Trailing junk recovers beside the keyword node and leaves later statements
available to parse. If that recovery reaches EOF, `parser::ParseDiagnostic`
reports `premature end of input` with a zero-width range at EOF.

`parser::reparse` tries token, nested-block, and top-level statement splices in
that order; `ReparseTier::Block` identifies a nested body splice. Block
fragments retain the owner's grammar context, including module `public`
declarations and struct fields. Block bodies reset enclosing bracket markers,
while indexing expressions inside them still parse `begin` and `end` normally.
Edits at body edges or in constructs with parser recovery fall back to an
enclosing tier or a full parse. Every successful splice preserves the tree and
diagnostics of `parse(edited_text)`.

Typed AST navigation is available through `fatou_parser::ast`. Its
`DocAttachment` view recognizes Julia's ordinary docstrings and exact
two-argument `@doc` calls. Standard ordinary and `raw` string payloads can be
decoded without evaluating Julia, with byte ranges mapped in both directions
between the decoded value and source; interpolated and custom payloads remain
opaque.

`parser::to_juliasyntax_sexpr` follows the pinned JuliaSyntax 2 encoding at
syntax version 1.15. Explicit macro names use `macro_name` nodes, modules carry
`v"1.15.0"` metadata, and compound assignment values use `op=` or `.op=` heads.
This diagnostic projection does not select a project's Julia syntax edition.

Decoded documentation can be parsed as Julia Markdown without invoking Julia:

```rust
use fatou_parser::documentation::{parse, reconstruct};

let source = "# API\n\nSee [`f`](@ref Main.f).\n";
let output = parse(source);
assert!(output.diagnostics.is_empty());
assert_eq!(reconstruct(&output.cst), source);
```

`fatou_parser::documentation::ast` provides typed navigation for Julia Markdown
blocks and inline content. It also classifies core Documenter links, fences, and
`jldoctest` info strings locally while preserving unknown extension syntax.
Inline constructs span paragraph continuations, including those in lists and
admonitions. Wrapped labels expose `Inline::SoftBreak` tokens, while inline code
and link destinations join continuations with spaces. The CST retains the
original newlines and container prefixes. A wrapped payload can contain several
content tokens, each with an exact decoded-text range; a range covering the
whole construct also includes the intervening prefixes.

Fences below list items and inside admonitions retain container prefixes as
whitespace trivia. Their `CodeBlock::content()` joins the `CODE_CONTENT` tokens
without those prefixes, preserving code indentation; each token has an exact
decoded-text range. Admonitions accept four-space and tab prefixes, including
mixed prefixes within one body. `content_range()` covers the whole body,
including prefixes between tokens, so consumers that transform the code should
map individual tokens rather than add offsets to that range.

## Status

This crate's API is still early and may change between releases; it is versioned
independently of the `fatou` CLI.

## Documentation

- [API documentation](https://docs.rs/fatou-parser)
- [fatou](https://fatou.dev), the project this crate is extracted from
