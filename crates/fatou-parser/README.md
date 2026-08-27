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

Typed AST navigation is available through `fatou_parser::ast`. Its
`DocAttachment` view recognizes Julia's ordinary docstrings and exact
two-argument `@doc` calls. Standard ordinary and `raw` string payloads can be
decoded without evaluating Julia, with byte ranges mapped back to the source;
interpolated and custom payloads remain opaque.

## Status

This crate's API is still early and may change between releases; it is versioned
independently of the `fatou` CLI.

## Documentation

- [API documentation](https://docs.rs/fatou-parser)
- [fatou](https://fatou.dev), the project this crate is extracted from
