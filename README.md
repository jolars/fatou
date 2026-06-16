# Fatou

A language server, formatter, and linter for [Julia](https://julialang.org),
written in Rust.

Fatou follows the rust-analyzer design — a lossless [`rowan`](https://crates.io/crates/rowan)
CST, [`salsa`](https://crates.io/crates/salsa) for incremental computation, and
[`lsp-server`](https://crates.io/crates/lsp-server) for the language-server
transport — and is modeled directly on the author's R tooling project, `arity`.

> **Status: early groundwork.** The full architecture is in place, but the
> parser covers only a small Julia subset, the formatter is currently a lossless
> passthrough, and no lint rules ship yet. See `TODO.md` for the roadmap and
> `AGENTS.md` for the design tenets.

## Usage

```sh
fatou parse <file.jl>          # print the CST (stdin if no file)
fatou format <file.jl>         # format to stdout (stdin if omitted)
fatou format --check <dir>     # check formatting; non-zero exit if any differ
fatou lint --check <dir>       # lint
fatou lsp                      # run the language server on stdio
```

Configuration lives in `fatou.toml` (`[format]` line_width/indent_width,
`[lint]` select/ignore).

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt -- --check
```

Or via [`task`](https://taskfile.dev): `task test`, `task lint`, `task format`.

## License

MIT — see [LICENSE](LICENSE).
