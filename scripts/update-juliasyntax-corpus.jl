#!/usr/bin/env julia
#
# Regenerate the pinned `expected.sexpr` files for the JuliaSyntax differential
# parser oracle (see `crates/fatou-parser/tests/juliasyntax_oracle.rs` and `AGENTS.md`).
#
# For every `crates/fatou-parser/tests/fixtures/oracle/<slug>/input.jl`, parse it with JuliaSyntax
# and write the s-expression rendering to a sibling `expected.sexpr`. A single
# Julia process handles the whole corpus so the interpreter startup cost is paid
# once. The pinned tool versions are recorded in `.juliasyntax-source`.
#
# Run via `scripts/update-juliasyntax-corpus.sh` (which resolves the repo root),
# or directly: `julia scripts/update-juliasyntax-corpus.jl`.

# JuliaSyntax comes from the active Julia environment: the root project's
# released package or the isolated development checkout selected by
# `refresh-juliasyntax-oracle.sh`.
include("juliasyntax-oracle.jl")

const REPO_ROOT = normpath(joinpath(@__DIR__, ".."))
const CORPUS_DIR =
    joinpath(REPO_ROOT, "crates", "fatou-parser", "tests", "fixtures", "oracle")

function main()
    isdir(CORPUS_DIR) || error("corpus dir not found: $CORPUS_DIR")
    slugs = sort(filter(readdir(CORPUS_DIR)) do entry
        isdir(joinpath(CORPUS_DIR, entry))
    end)

    written = 0
    for slug in slugs
        input = joinpath(CORPUS_DIR, slug, "input.jl")
        isfile(input) || continue
        edition = joinpath(CORPUS_DIR, slug, "edition.txt")
        version = isfile(edition) ? VersionNumber(strip(read(edition, String))) : ORACLE_SYNTAX_VERSION
        sexpr = render_oracle(read(input, String); version)
        open(joinpath(CORPUS_DIR, slug, "expected.sexpr"), "w") do io
            print(io, sexpr)
            print(io, '\n')
        end
        written += 1
    end

    write_oracle_source(joinpath(CORPUS_DIR, ".juliasyntax-source"))

    println("wrote $written expected.sexpr file(s) to $CORPUS_DIR")
end

main()
