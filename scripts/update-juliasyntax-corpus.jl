#!/usr/bin/env julia
#
# Regenerate the pinned `expected.sexpr` files for the JuliaSyntax differential
# parser oracle (see `tests/juliasyntax_oracle.rs` and `AGENTS.md`).
#
# For every `tests/fixtures/oracle/<slug>/input.jl`, parse it with JuliaSyntax
# and write the s-expression rendering to a sibling `expected.sexpr`. A single
# Julia process handles the whole corpus so the interpreter startup cost is paid
# once. The pinned tool versions are recorded in `.juliasyntax-source`.
#
# Run via `scripts/update-juliasyntax-corpus.sh` (which resolves the repo root),
# or directly: `julia scripts/update-juliasyntax-corpus.jl`.

using JuliaSyntax

const REPO_ROOT = normpath(joinpath(@__DIR__, ".."))
const CORPUS_DIR = joinpath(REPO_ROOT, "tests", "fixtures", "oracle")

function render(src::AbstractString)
    # `ignore_errors=true` lets us pin output for inputs Fatou parses but Julia
    # rejects; the harness skips error cases by default, but a pinned rendering
    # keeps the corpus uniform.
    node = JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; ignore_errors = true)
    return sprint(print, node)
end

function main()
    isdir(CORPUS_DIR) || error("corpus dir not found: $CORPUS_DIR")
    slugs = sort(filter(readdir(CORPUS_DIR)) do entry
        isdir(joinpath(CORPUS_DIR, entry))
    end)

    written = 0
    for slug in slugs
        input = joinpath(CORPUS_DIR, slug, "input.jl")
        isfile(input) || continue
        sexpr = render(read(input, String))
        open(joinpath(CORPUS_DIR, slug, "expected.sexpr"), "w") do io
            print(io, sexpr)
            print(io, '\n')
        end
        written += 1
    end

    open(joinpath(CORPUS_DIR, ".juliasyntax-source"), "w") do io
        println(io, "julia_version=", VERSION)
        println(io, "juliasyntax_version=", pkgversion(JuliaSyntax))
    end

    println("wrote $written expected.sexpr file(s) to $CORPUS_DIR")
end

main()
