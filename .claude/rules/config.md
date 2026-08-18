---
paths:
  - "src/config.rs"
  - "src/julia_version.rs"
  - "src/file_discovery.rs"
  - "fatou.toml"
  - "tests/config_discovery.rs"
  - "docs/src/guide/configuration.md"
  - "docs/src/reference/configuration.md"
---

# Config rules

Scope: `src/config.rs`, `src/julia_version.rs`, and `src/file_discovery.rs`.
`src/config.rs` defines the `fatou.toml` schema, defaults, and discovery.
Discovery walks up from an anchor directory, then falls back to
`$FATOU_CONFIG`, then the global user config. Every command honors it, so **a
schema change affects format, lint, and the LSP at once**.

## The schema today

- top-level: `exclude` (gitignore-style) and `extend-exclude` (adds to it, kept
  separate so that if `exclude` ever gains built-in defaults, setting it
  replaces them while `extend-exclude` only ever adds).
- `[format]`: `line-width` (default 92), `indent-width` (4), `line-ending`.
- `[lint]`: `select` (allowlist; when `Some`, only those run), `ignore`,
  `[lint.severity]` per-rule overrides, and `[lint.rules.<id>]` option tables.
- `[julia]`: `version` — the target Julia range for `julia-version-compat`, with
  room to grow into environment-resolution overrides.

Defaults follow common Julia conventions, not Rust ones.

## Conventions when extending it

- Structs are `#[serde(deny_unknown_fields, rename_all = "kebab-case")]`, so a
  user's typo is an **error, not a silent no-op**. TOML keys are kebab-case, and
  `rename_all` is what keeps a table name matching a rule's public ID.
- **Strictness is deliberately asymmetric.** `select`/`ignore`/`[lint.severity]`
  are *data* (a list of IDs the user typed): an unknown ID there is reported at
  lint time. `[lint.rules.<id>]` is *schema*: a mistyped ID or key there is a
  config **parse error**.
- **`replace` vs `extend` is a pair**, not a one-off: `exclude`/`extend-exclude`
  and `functions`/`extend-functions` are the same idiom. Follow it for any new
  set-valued option.
- A field that derives `Default` and seeds a non-empty built-in set must write
  `impl Default` **by hand** — the derive hands back an empty map and would
  silently disable the feature whenever the table is absent.
- **A new key needs a reason for its level.** Excludes are top-level, not under
  `[format]`, because format and lint share one walk. `[julia] version` is
  top-level-ish because the Julia support range is a *project fact*, not a lint
  option.
- **The library API takes a fully-resolved `FormatStyle`.** Config resolution is
  the caller's job (CLI, LSP), so every walk honors the same excludes, and the
  engine crate never learns about `fatou.toml`. The `From<&FormatConfig>` impl
  lives in `config.rs` for exactly that reason.
- A setting that is a fact about the **machine** belongs in editor settings
  (`src/lsp/config.rs`), not `fatou.toml`.

## Julia versions are not Cargo semver

`julia_version.rs` parses Julia's `Pkg` compat grammar: a bare `"1.6"` carries
an implicit caret (`>= 1.6.0, < 2.0.0`), `-` spells an inclusive range, and a
comma spells a union. `parse_compat` collapses a union to its lowest floor and
highest ceiling rather than modeling disjoint intervals — we only need the
overall range for compat checking. Do not reach for a semver crate here.

## When you change it

Update `tests/config_discovery.rs` and the hand-written docs pages
(`docs/src/guide/configuration.md`, `docs/src/reference/configuration.md` — both
hand-written, unlike the CLI and rule references).
