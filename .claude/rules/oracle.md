---
paths:
  - "crates/fatou-parser/src/parser/sexpr.rs"
  - "crates/fatou-parser/tests/juliasyntax_oracle.rs"
  - "crates/fatou-parser/tests/fixtures/oracle/**/*"
  - "crates/fatou-parser/tests/oracle/**/*"
  - "scripts/*juliasyntax*"
  - "Project.toml"
  - "Manifest.toml"
---

# JuliaSyntax oracle rules

The differential oracle for the parser is **JuliaSyntax.jl**, the official Julia
parser (itself a lossless green-tree design). The full workflow for closing a
gap is the `parser-parity` skill.

## The projector is a diagnostic, never a fix

`crates/fatou-parser/src/parser/sexpr.rs` (also `fatou parse --to sexpr`) walks
the CST and emits JuliaSyntax's s-expression shape.

- **Never patch the projector to make a case pass.** A divergence means the CST
  — or the projector's encoding translation — is wrong. Fix the parser.
- The harness (`tests/juliasyntax_oracle.rs`) diffs each fixture against a
  pinned `expected.sexpr`, so **no Julia is needed at test time** and it is
  CI-safe. Regressions are gated by allowlists; a newly-passing case is
  **ratcheted in** so it cannot regress.
- Two corpora feed it: the curated dir corpus
  (`crates/fatou-parser/tests/fixtures/oracle/`) and the harvested JuliaSyntax
  sub-corpus (`juliasyntax.jsonl`).

## The version pin is the contract

Regenerating the pinned corpus (`scripts/update-juliasyntax-corpus.sh`) is the
one task that needs Julia.

- JuliaSyntax is pinned **exactly** (`=0.4.10`) in the root `Project.toml`
  `[compat]`, with a committed `Manifest.toml`. The regen scripts mirror the
  resolved versions (Julia's and JuliaSyntax's) into
  `crates/fatou-parser/tests/fixtures/oracle/.juliasyntax-source`.
- Julia packages are **Pkg-managed, not Nix-managed**: `devenv.nix` provides
  only the bare `julia-bin` interpreter and the shell exports `JULIA_PROJECT=@.`.
  This replaced nixpkgs' `withPackages`, which resolved an old registry snapshot
  and pinned JuliaSyntax by accident, defeating the exact-version contract.
- The regen scripts just `using JuliaSyntax` from the *active* environment.
  **They must not force-activate or instantiate the root project**: the
  web-container `SessionStart` hook provisions JuliaSyntax differently (a pinned
  git checkout on `JULIA_LOAD_PATH`), avoiding the Pkg/registry access that
  container lacks.
- **Re-running a regen script must leave every file it did not target
  byte-identical.** Any other diff is a version mismatch, not a parser change —
  a different Julia or JuliaSyntax rewrites unrelated fixtures and buries the
  intended change.

To bump the oracle: edit the `[compat]` bound in `Project.toml`, re-resolve
(`julia --project=. -e 'using Pkg; Pkg.update("JuliaSyntax")'`), re-run both
regen scripts, then re-triage.
