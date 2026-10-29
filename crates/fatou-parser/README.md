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
Fences below list items retain the list prefix as whitespace trivia. Their
`CodeBlock::content()` joins the `CODE_CONTENT` tokens without that prefix; each
token has an exact decoded-text range. `content_range()` covers the whole body,
including indentation between tokens, so consumers that transform the code
should map individual tokens rather than add offsets to that range.

## Status

This crate's API is still early and may change between releases; it is versioned
independently of the `fatou` CLI.

## Documentation

- [API documentation](https://docs.rs/fatou-parser)
- [fatou](https://fatou.dev), the project this crate is extracted from
