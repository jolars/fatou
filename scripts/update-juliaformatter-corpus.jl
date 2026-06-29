#!/usr/bin/env julia
#
# Regenerate the pinned `expected.jl` files for the JuliaFormatter.jl
# differential formatter oracle (see `tests/juliaformatter_oracle.rs` and
# `AGENTS.md`).
#
# For every `tests/fixtures/formatter/<slug>/input.jl`, run JuliaFormatter
# (DefaultStyle) over it and write the formatted result to a sibling
# `expected.jl`. A single Julia process handles the whole corpus so the
# interpreter startup cost is paid once. The pinned tool versions are recorded
# in `.juliaformatter-source`.
#
# `format_text` is JuliaFormatter's string-in/string-out entry point; it is not
# exported, so it is called qualified. `margin`/`indent` are pinned explicitly
# (DefaultStyle's defaults) so an upstream default change can't silently shift
# the corpus.
#
# Run via `scripts/update-juliaformatter-corpus.sh` (which resolves the repo
# root), or directly: `julia scripts/update-juliaformatter-corpus.jl`.

import JuliaFormatter

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
        formatted = JuliaFormatter.format_text(read(input, String); margin = 92, indent = 4)
        open(joinpath(CORPUS_DIR, slug, "expected.jl"), "w") do io
            print(io, formatted)
        end
        written += 1
    end

    open(joinpath(CORPUS_DIR, ".juliaformatter-source"), "w") do io
        println(io, "julia_version=", VERSION)
        println(io, "juliaformatter_version=", pkgversion(JuliaFormatter))
    end

    println("wrote $written expected.jl file(s) to $CORPUS_DIR")
end

main()
