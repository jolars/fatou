#!/usr/bin/env julia
#
# Harvest parser test inputs from JuliaSyntax.jl's own `test/parser.jl` into a
# pinned JSONL corpus for the differential parser oracle (track 1; see
# `AGENTS.md` and `tests/juliasyntax_oracle.rs`).
#
# JuliaSyntax's `test/parser.jl` holds ~690 hand-curated `"source" => "(sexpr)"`
# pairs, organized per *production rule*. We cannot reuse their expected strings
# directly — they are produced per-production (no `(toplevel …)` root) and with
# `keep_parens=true`. So we harvest only the **input strings** and regenerate
# the expected s-expression ourselves via `parseall` (the same pinned oracle the
# rest of the corpus uses), keeping the projector and harness as the single
# source of truth.
#
# Inputs whose `parseall` output contains an error node, and empty/whitespace
# inputs, are skipped: those belong to the deferred error-shape-parity phase.
#
# Output: `tests/fixtures/oracle/juliasyntax.jsonl`, one
# `{"slug","input","expected"}` object per line, sorted by slug. Pinned to the
# same JuliaSyntax version recorded in `.juliasyntax-source`. Re-run on a
# version bump (then re-triage `tests/oracle/juliasyntax-blocked.txt`).

using JuliaSyntax
using SHA

const REPO_ROOT = normpath(joinpath(@__DIR__, ".."))
const OUT_PATH = joinpath(REPO_ROOT, "tests", "fixtures", "oracle", "juliasyntax.jsonl")

"Recursively collect the left-hand input string of every `… => …` pair."
function collect_inputs!(into::Set{String}, ex)
    if ex isa Expr
        if ex.head === :call && length(ex.args) == 3 && ex.args[1] === :(=>)
            lhs = ex.args[2]
            if lhs isa String
                push!(into, lhs)
            elseif lhs isa Expr && lhs.head === :tuple
                # `(opts, "input") => …` — pull the string out of the tuple.
                for a in lhs.args
                    a isa String && push!(into, a)
                end
            end
        end
        for a in ex.args
            collect_inputs!(into, a)
        end
    end
    return into
end

function harvest_inputs()
    src_path = joinpath(pkgdir(JuliaSyntax), "test", "parser.jl")
    isfile(src_path) || error("JuliaSyntax test/parser.jl not found at $src_path")
    ast = Meta.parseall(read(src_path, String); filename = src_path)
    # Scope to the `tests = [...]` binding so helper-definition `=>` and the
    # broken-code spec list are not swept in.
    tests_value = nothing
    for stmt in ast.args
        if stmt isa Expr && stmt.head === :(=) && stmt.args[1] === :tests
            tests_value = stmt.args[2]
            break
        end
    end
    tests_value === nothing && error("could not find `tests = [...]` in $src_path")
    sort!(collect(collect_inputs!(Set{String}(), tests_value)))
end

function render(src::AbstractString)
    node = JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; ignore_errors = true)
    sprint(print, node)
end

slug(src::AbstractString) = "js-" * bytes2hex(sha256(codeunits(src)))[1:8]

function json_str(s::AbstractString)
    io = IOBuffer()
    print(io, '"')
    for c in s
        if c == '"'
            print(io, "\\\"")
        elseif c == '\\'
            print(io, "\\\\")
        elseif c == '\n'
            print(io, "\\n")
        elseif c == '\t'
            print(io, "\\t")
        elseif c == '\r'
            print(io, "\\r")
        elseif c == '\f'
            print(io, "\\f")
        elseif c == '\b'
            print(io, "\\b")
        elseif c < ' '
            print(io, "\\u", lpad(string(UInt32(c), base = 16), 4, '0'))
        else
            print(io, c)
        end
    end
    print(io, '"')
    String(take!(io))
end

function main()
    inputs = harvest_inputs()
    rows = Tuple{String,String,String}[]
    (skipped_error, skipped_empty, skipped_throw, skipped_invalid, dup) = (0, 0, 0, 0, 0)
    seen = Set{String}()
    for input in inputs
        if isempty(strip(input))
            skipped_empty += 1
            continue
        end
        # JuliaSyntax includes deliberately-malformed unicode (bidi, overlong)
        # lexer cases; those are not parser-parity cases and cannot be encoded as
        # JSON, so skip non-UTF-8 inputs.
        if !isvalid(input)
            skipped_invalid += 1
            continue
        end
        sexpr = try
            render(input)
        catch
            skipped_throw += 1
            continue
        end
        if occursin("(error", sexpr) || !isvalid(sexpr)
            skipped_error += 1
            continue
        end
        s = slug(input)
        if s in seen
            dup += 1
            continue
        end
        push!(seen, s)
        push!(rows, (s, input, sexpr))
    end
    sort!(rows; by = first)

    open(OUT_PATH, "w") do io
        for (s, input, sexpr) in rows
            println(
                io,
                "{\"slug\":",
                json_str(s),
                ",\"input\":",
                json_str(input),
                ",\"expected\":",
                json_str(sexpr),
                "}",
            )
        end
    end

    println("harvested $(length(inputs)) inputs from JuliaSyntax test/parser.jl")
    println(
        "wrote $(length(rows)) cases to $OUT_PATH " *
        "(skipped: $skipped_error error, $skipped_empty empty, " *
        "$skipped_invalid non-utf8, $skipped_throw throwing, $dup duplicate)",
    )
end

main()
