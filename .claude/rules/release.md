---
paths:
  - ".github/workflows/*.yml"
  - "versionary.jsonc"
  - "editors/code/src/**/*.ts"
  - "editors/code/*.json*"
  - "npm/**/package.json"
  - "npm/**/*.js"
  - "packaging/**/*"
  - "scripts/*installer*"
  - "scripts/aur_push.sh"
  - "pyproject.toml"
  - "Cargo.toml"
  - "crates/*/Cargo.toml"
  - "deny.toml"
  - "audit.toml"
---

# Distribution and release rules

Releases are fully automated off Conventional Commits; the commit type picks the
version.

## Never hand-edit

**`CHANGELOG.md` and every version field are generated** — `versionary`
(`versionary.jsonc`) overwrites your edit. That includes
`npm/fatou-cli/package.json`'s own version *and* every `optionalDependencies`
entry, which the release PR propagates. Write a good conventional commit
instead. `bump-minor-pre-major` is set, so pre-1.0 breaking changes land as
**minor** bumps.

## Four packages, routed by path

| Package | Tag stream |
| --- | --- |
| root CLI (`fatou`) | bare `v*` |
| `crates/fatou-parser` | `fatou-parser-v*` |
| `crates/fatou-formatter` | `fatou-formatter-v*` |
| `editors/code` (`fatou-code`) | `follows` the CLI |

Paths under `editors/` and `crates/` are **excluded from the CLI's version
calculation**, so **keep commits atomic per area** — a commit spanning the root
crate and a member crate produces muddled per-crate changelogs. Member tags are
prefixed so the `v*` filters match only the CLI stream, and **only the CLI
stream carries GitHub release assets** (`startsWith(tag, 'v')` guards it).

## The pipeline

Push to `main` → `build-and-test.yml` (cross-platform build/test, the **wasm
job**, `cargo-audit` + `cargo-deny`) passes → `versionary` opens or updates a
release PR. Merging it tags and fans out to `packages.yml`, then the VS
Code/Open VSX, crates.io, npm, PyPI, and AUR publishes.

`publish-crates.yml` publishes every workspace crate not yet on crates.io, in
dependency order — so a member-crate bump ships on the next CLI tag.

## Quality gates

Treat CI as the source of truth. `lint.yml` runs clippy `-D warnings` and the
rustfmt check; `build-and-test.yml` adds the wasm build, minimal versions, and
supply chain. A dependency change must stay clean under `cargo-audit` and
`cargo-deny` (`deny.toml`, `audit.toml`), both of which gate the release PR.

**Declare honest lower bounds.** A published crate's version requirements are a
contract: `fatou-parser` and `fatou-formatter` go to crates.io, and the
committed lockfile hides an understated bound because it resolves to the latest
patch. The `minimal-versions` job resolves direct dependencies down to their
declared minimums and compiles, so **raise a requirement when you use an API
newer than it** — the bump belongs in the same commit as the code that needs it. Locally,
`devenv.nix` declares the git hooks (clippy, rustfmt, biome).

**The wasm job is a real constraint, not a formality**: it is the only thing
keeping `fatou-parser` and `fatou-formatter` free of filesystem, process,
thread, and clock dependencies, which a dprint plugin needs.

## Surfaces

- `editors/code` — TypeScript VS Code extension, **biome**-gated
  (`biome.json`). At publish time a platform binary is downloaded into the
  extension and packaged per target; at runtime the client can also resolve
  `fatou` from PATH — which is the NixOS path, where a downloaded binary would
  not run. **Don't make the bundled binary load-bearing.**
- `npm/fatou-cli` — a launcher whose `optionalDependencies` pull one
  `@fatou-cli/<platform>` package per target, generated from
  `npm/platform-template`.
- `pyproject.toml` — the PyPI package, built by maturin.
- `packaging/aur` — the `PKGBUILD`; `scripts/aur_push.sh` (`task aur:push`) is
  the manual CI fallback.
- `scripts/fatou-installer.{sh,ps1}` — the curl-to-shell installers.

## Smoke tests

`smoke-test.yml` runs `fatou debug format` over real Julia repos and files one
deduped issue per (repo, failure category). Its report format is a contract with
`src/debug.rs` (`.claude/rules/formatter.md`); triage with the
`smoke-test-triage` skill.
