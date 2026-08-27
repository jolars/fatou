# Changelog

## [0.6.0](https://github.com/jolars/fatou/compare/fatou-formatter-v0.5.0...fatou-formatter-v0.6.0) (2026-08-27)

### Features
- **formatter:** verify formatting preserves the program with ast_shape ([`3f4e9c9`](https://github.com/jolars/fatou/commit/3f4e9c93d671071b529089ed9c9cedc81b44f9a1))

### Bug Fixes
- **formatter:** preserve unary numeric calls ([`09320a9`](https://github.com/jolars/fatou/commit/09320a9e1b847ce2863031955f0ccb3f5efd36fc))
- **formatter:** preserve operator call commas ([`cbe6d31`](https://github.com/jolars/fatou/commit/cbe6d31edd16a9411e8741493aaa00e70b0968a7))
- **formatter:** preserve trailing semicolons ([`3e9a47f`](https://github.com/jolars/fatou/commit/3e9a47f5495848d69b597dcd52e448b00e202f78))
- **formatter:** preserve macro argument gaps ([`26297a8`](https://github.com/jolars/fatou/commit/26297a8b4d67fe51270467467dbae54af544129a))
- **formatter:** preserve where brace shapes ([`52d7fbc`](https://github.com/jolars/fatou/commit/52d7fbc3d256fec4e42d3b1da368c87f3460f1a9))
- **formatter:** preserve grouped hex width ([`05b2f72`](https://github.com/jolars/fatou/commit/05b2f7237dd4f0982028928e9798d9b65db11219)), closes [#92](https://github.com/jolars/fatou/issues/92)

### Dependencies
- updated crates/fatou-parser to v0.6.0

## [0.5.0](https://github.com/jolars/fatou/compare/fatou-formatter-v0.4.0...fatou-formatter-v0.5.0) (2026-08-26)

### Features
- **formatter:** align trailing comments ([`132272b`](https://github.com/jolars/fatou/commit/132272b3ccfe076a8eb8eff261e021e524d9c9b6))

## [0.4.0](https://github.com/jolars/fatou/compare/fatou-formatter-v0.3.3...fatou-formatter-v0.4.0) (2026-08-25)

### Features
- **formatter:** hug bracketed splats ([`d5da2eb`](https://github.com/jolars/fatou/commit/d5da2eb121b09747fb87e336bd9cb63938d19989))

### Dependencies
- updated crates/fatou-parser to v0.5.0

## [0.3.3](https://github.com/jolars/fatou/compare/fatou-formatter-v0.3.2...fatou-formatter-v0.3.3) (2026-08-16)

### Performance Improvements
- **formatter:** stop copying the print stack per fit probe ([`9f317e3`](https://github.com/jolars/fatou/commit/9f317e32e84603fdb8538c7282078d940b93d3f4))

### Dependencies
- updated crates/fatou-parser to v0.4.1

## [0.3.2](https://github.com/jolars/fatou/compare/fatou-formatter-v0.3.1...fatou-formatter-v0.3.2) (2026-08-13)

### Dependencies
- updated crates/fatou-parser to v0.4.0

## [0.3.1](https://github.com/jolars/fatou/compare/fatou-formatter-v0.3.0...fatou-formatter-v0.3.1) (2026-08-09)

### Dependencies
- updated crates/fatou-parser to v0.3.0

## [0.3.0](https://github.com/jolars/fatou/compare/fatou-formatter-v0.2.1...fatou-formatter-v0.3.0) (2026-08-07)

### Features
- **parser:** accept `∈` as iteration separator ([`1f5d8d9`](https://github.com/jolars/fatou/commit/1f5d8d90bf9e7dd55d73e1b71a567870145adde7))

### Dependencies
- updated crates/fatou-parser to v0.2.0

## [0.2.1](https://github.com/jolars/fatou/compare/fatou-formatter-v0.2.0...fatou-formatter-v0.2.1) (2026-08-07)

### Bug Fixes
- **deps:** require `fatou-parser` 0.1.1 ([`2f6a764`](https://github.com/jolars/fatou/commit/2f6a764f2d21185f8d73677a82b65500e973a6d3))

## [0.2.0](https://github.com/jolars/fatou/compare/fatou-formatter-v0.1.0...fatou-formatter-v0.2.0) (2026-08-07)

### Features
- **formatter:** add `serde` and `schema` features ([`010f07c`](https://github.com/jolars/fatou/commit/010f07cde08ab22b02932512fecb74188e994182))
