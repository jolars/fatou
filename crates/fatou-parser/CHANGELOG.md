# Changelog

## [0.5.0](https://github.com/jolars/fatou/compare/fatou-parser-v0.4.1...fatou-parser-v0.5.0) (2026-08-25)

### Features
- **parser:** parse nested macro loop arguments ([`1a90dde`](https://github.com/jolars/fatou/commit/1a90dde91544496331d5a5be20b1e998e375a851))
- **parser:** parse dotted Unicode unary ops ([`406247e`](https://github.com/jolars/fatou/commit/406247e8d66e52df535721870d385fa099e8c999))
- **parser:** parse spaced operator calls ([`6c3fdf0`](https://github.com/jolars/fatou/commit/6c3fdf066cff69e8d6717384c7d0f95424277820))
- **parser:** recover parenthesized leading commas ([`1e08166`](https://github.com/jolars/fatou/commit/1e08166c8d00ec3f58d7afa2b8216dbc21a1a194))
- **parser:** recover leading empty list slots ([`5b5ef49`](https://github.com/jolars/fatou/commit/5b5ef49462952a17b980b3705628e04123153c1e))
- **parser:** parse var forward declarations ([`9dd14cb`](https://github.com/jolars/fatou/commit/9dd14cbb2af02e16564328c2f4aefff8a8c34558))
- **parser:** recover quoted import names ([`26ffb4d`](https://github.com/jolars/fatou/commit/26ffb4d9ce421bff93eeb989ad11370acfcf3474))
- **parser:** recover parenthesized macro signatures ([`3131255`](https://github.com/jolars/fatou/commit/3131255e088ad96407f2bbd5b6ade6cb0515f43c))
- **parser:** recover bare colon-eq and dot ([`e379a8e`](https://github.com/jolars/fatou/commit/e379a8e1ce5b65641d10915f2ec0ff13c0e7cb07))
- **parser:** parse command macro numeric args ([`d680355`](https://github.com/jolars/fatou/commit/d680355cf5114435385c1f161d0141b325157eeb))
- **parser:** parse parenthesized lambda params ([`0809d8c`](https://github.com/jolars/fatou/commit/0809d8c703b1b43c0b6c291c5e199511da4f73b6))
- **parser:** parse generator parameters ([`e59f76e`](https://github.com/jolars/fatou/commit/e59f76e425a5f2d70d646ee5d8921867676c2b21))
- **parser:** finish JuliaSyntax 1.0 migration ([`f53225d`](https://github.com/jolars/fatou/commit/f53225df9834dd0d5d8006a784c7c33b623f4ff2))

### Bug Fixes
- **parser:** lex U+1F8B2 as an arrow operator ([`ede624d`](https://github.com/jolars/fatou/commit/ede624dbc3c29aa904599158a6b0f9f562ea06aa))
- **parser:** project every field-access right operand ([`b544757`](https://github.com/jolars/fatou/commit/b544757e310712fcbd0610c9806a3925e937084e))
- **parser:** keep `do` clauses off non-call heads ([`2c3295c`](https://github.com/jolars/fatou/commit/2c3295cb433054bd01cf60145c69e6f261d7ba31))
- **parser:** report an unclosed leading-comma paren ([`9c8208d`](https://github.com/jolars/fatou/commit/9c8208d07ede2e125e32d8e8b47782bf7ec6f3d1))
- **parser:** stop operators reaching past a line comment ([`3d410b8`](https://github.com/jolars/fatou/commit/3d410b8aea1dc2398e0e4f6b85d8fc402831b9eb))
- **parser:** decode raw triple-string quotes ([`3cac3ae`](https://github.com/jolars/fatou/commit/3cac3ae8643df527c3551680c48422f5e34fe7e8))
- **parser:** escape triple-string quotes ([`ed55f05`](https://github.com/jolars/fatou/commit/ed55f05effc7a125dd7dace906514d789aa48969))
- **parser:** preserve nested matrix row order ([`003e88e`](https://github.com/jolars/fatou/commit/003e88e2f8f8b1955a938a6ce4558acfed41ca1d))
- **parser:** report invalid aliases ([`c08174d`](https://github.com/jolars/fatou/commit/c08174dd8fc5bbf9dca27fcb01c4db1f374d417d))

### Performance Improvements
- **parser:** take an operator's last char in `O(1)` ([`fd6acfd`](https://github.com/jolars/fatou/commit/fd6acfd6e37fc6611d3b405ca860381e61b712fc))

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
