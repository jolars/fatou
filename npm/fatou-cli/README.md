# fatou-cli

[Fatou](https://github.com/jolars/fatou) is a language server, formatter, and
linter for the [Julia](https://julialang.org) programming language.

## Install

```sh
npm install -g fatou-cli
```

This installs the `fatou` command globally. The package detects your platform at
install time and pulls in a prebuilt binary via npm's optional dependencies---no
Rust toolchain or postinstall download required.

You can also use it without a global install:

```sh
npx fatou-cli format file.jl
```

## Usage

```sh
fatou parse file.jl             # print the CST (stdin if no file)
fatou format file.jl            # format to stdout (stdin if omitted)
fatou format --check path/      # check formatting; non-zero exit if any differ
fatou lint --check path/        # lint
fatou lsp                       # run the language server on stdio
```

See `fatou --help` and the [documentation](https://github.com/jolars/fatou) for
the full feature list and configuration reference.

## Supported platforms

Prebuilt binaries are shipped for:

- Linux x64 (glibc and musl)
- Linux arm64 (glibc and musl)
- macOS x64 (Intel) and arm64 (Apple Silicon)
- Windows x64 and arm64

If your platform isn't covered, install via
[Cargo](https://crates.io/crates/fatou),
[PyPI](https://pypi.org/project/fatou/), or one of the other methods listed at
<[https://github.com/jolars/fatou](https://github.com/jolars/fatou)>.

## License

MIT---see [LICENSE](./LICENSE).
