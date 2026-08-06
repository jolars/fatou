# fatou-formatter

A deterministic, rule-based formatter for the Julia language: a
Wadler/Prettier-style document IR printed by a single best-fit layout engine.

This is the formatting engine of [fatou](https://fatou.dev), extracted into
its own crate so embedders (such as a dprint Wasm plugin) can use it without
the CLI's filesystem and process machinery. The crate builds for
`wasm32-unknown-unknown`: it uses no filesystem, process, thread, or clock
facilities.

## Usage

```rust
use fatou_formatter::format;

let formatted = format("f( x )=x+1").unwrap();
assert_eq!(formatted, "f(x) = x + 1\n");
```

`format_node` and `format_range` accept an already-parsed
[fatou-parser](https://crates.io/crates/fatou-parser) CST, whole or a byte
range of it.

## Status

This crate's API is still early and may change between releases; it is
versioned independently of the `fatou` CLI.

## Documentation

- [API documentation](https://docs.rs/fatou-formatter)
- [fatou](https://fatou.dev), the project this crate is extracted from
