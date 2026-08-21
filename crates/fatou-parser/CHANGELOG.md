# Changelog

## [0.4.2](https://github.com/jolars/fatou/compare/fatou-parser-v0.4.1...fatou-parser-v0.4.2) (2026-08-21)

### Bug Fixes
- **parser:** report invalid aliases ([`c08174d`](https://github.com/jolars/fatou/commit/c08174dd8fc5bbf9dca27fcb01c4db1f374d417d))

## [0.4.1](https://github.com/jolars/fatou/compare/fatou-parser-v0.4.0...fatou-parser-v0.4.1) (2026-08-16)

### Bug Fixes
- **parser:** cast the five wrapped kinds `Expr` dropped to `Other` ([`1347ed5`](https://github.com/jolars/fatou/commit/1347ed5b2e55410def89505fe1a07640eccde50f))

### Performance Improvements
- **parser:** borrow token text from the source ([`ac7f2e2`](https://github.com/jolars/fatou/commit/ac7f2e2a7ea6c68f7f24452c78d9b5df9780525c))
- **parser:** fold docstrings in one linear pass ([`4f55e60`](https://github.com/jolars/fatou/commit/4f55e6028d9ccb4893b770ccf1b2f5419892748a))

## [0.4.0](https://github.com/jolars/fatou/compare/fatou-parser-v0.3.0...fatou-parser-v0.4.0) (2026-08-13)

### Features
- **parser:** expose `string_value` for literal decoding ([`e4c4529`](https://github.com/jolars/fatou/commit/e4c45299c6cf0cf4829efcefb8163262f19982dd))

## [0.3.0](https://github.com/jolars/fatou/compare/fatou-parser-v0.2.0...fatou-parser-v0.3.0) (2026-08-09)

### Features
- **linter:** add `kwarg-default-mismatch` ([`d395287`](https://github.com/jolars/fatou/commit/d395287312b79f57a77a4895d207bd3ee4a8e252))

## [0.2.0](https://github.com/jolars/fatou/compare/fatou-parser-v0.1.1...fatou-parser-v0.2.0) (2026-08-07)

### Features
- **linter:** add `redundant-boolean` ([`66fd6bc`](https://github.com/jolars/fatou/commit/66fd6bc6b8022875d34f66c98d651aae82fff513))
- **parser:** accept `∈` as iteration separator ([`1f5d8d9`](https://github.com/jolars/fatou/commit/1f5d8d90bf9e7dd55d73e1b71a567870145adde7))
- **parser:** add `for outer i` iteration spec ([`69c7e2a`](https://github.com/jolars/fatou/commit/69c7e2ac09c64f526726f8148c422c33ac8218ef))
- **parser:** end keyword stmt at a generator `for` ([`215ecaf`](https://github.com/jolars/fatou/commit/215ecafeefc507f196ad6729c5ba8be393da252a))
