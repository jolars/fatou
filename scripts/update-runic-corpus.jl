#!/usr/bin/env julia
#
# Regenerate the pinned `expected.jl` files for the Runic differential formatter
# oracle (see `tests/runic_oracle.rs` and `AGENTS.md`).
#
# For every `tests/fixtures/formatter/<slug>/input.jl`, run Runic over it and
# write the formatted result to a sibling `expected.jl`. A single Julia process
# handles the whole corpus so the interpreter startup cost is paid once. The
# pinned tool versions are recorded in `.runic-source`.
#
# Run via `scripts/update-runic-corpus.sh` (which resolves the repo root), or
# directly: `julia scripts/update-runic-corpus.jl`.

using Runic

const REPO_ROOT = normpath(joinpath(@__DIR__, ".."))
const CORPUS_DIR = joinpath(REPO_ROOT, "tests", "fixtures", "formatter")

function main()
    isdir(CORPUS_DIR) || error("corpus dir not found: $CORPUS_DIR")
    slugs = sort(filter(readdir(CORPUS_DIR)) do entry
        isdir(joinpath(CORPUS_DIR, entry))
    end)

    written = 0
    for slug in slugs
        input = joinpath(CORPUS_DIR, slug, "input.jl")
        isfile(input) || continue
        formatted = Runic.format_string(read(input, String))
        open(joinpath(CORPUS_DIR, slug, "expected.jl"), "w") do io
            print(io, formatted)
        end
        written += 1
    end

    open(joinpath(CORPUS_DIR, ".runic-source"), "w") do io
        println(io, "julia_version=", VERSION)
        println(io, "runic_version=", pkgversion(Runic))
    end

    println("wrote $written expected.jl file(s) to $CORPUS_DIR")
end

main()
