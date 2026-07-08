#!/usr/bin/env julia
# Warm-loop throughput harness for Runic and JuliaFormatter, the Julia-native
# counterparts to Fatou. It mirrors the Rust harness (benches/format_compare.rs)
# and emits the same per-file JSON schema so `bench/compare_format.sh` can merge
# them.
#
# Both tools expose a pure String -> String formatter (Runic.format_string,
# JuliaFormatter.format_text). We load the package once, warm it up (which pays
# the JIT/compile cost outside the measured region), then time N calls in a
# loop. That is what makes the comparison against Fatou fair: no process startup
# and no first-call compilation counted.
#
# Usage:
#   julia --startup-file=no bench/julia_bench.jl <filelist> <iterations> <warmup> <out.json>
# where <filelist> has one source path per line.

# --- minimal JSON writer (no JSON.jl dependency in the pinned env) -----------

json_escape(s::AbstractString) = sprint() do io
    for c in s
        if c == '"'
            print(io, "\\\"")
        elseif c == '\\'
            print(io, "\\\\")
        elseif c == '\n'
            print(io, "\\n")
        elseif c == '\r'
            print(io, "\\r")
        elseif c == '\t'
            print(io, "\\t")
        elseif c < ' '
            print(io, "\\u", lpad(string(UInt(c), base = 16), 4, '0'))
        else
            print(io, c)
        end
    end
end

to_json(x::AbstractString) = string('"', json_escape(x), '"')
to_json(x::Bool) = x ? "true" : "false"
to_json(x::Integer) = string(x)
to_json(x::AbstractFloat) = isfinite(x) ? string(x) : "null"
to_json(::Nothing) = "null"
to_json(v::AbstractVector) = string('[', join(map(to_json, v), ','), ']')
function to_json(d::AbstractDict)
    parts = ["$(to_json(string(k))):$(to_json(v))" for (k, v) in d]
    string('{', join(parts, ','), '}')
end

# --- timing ------------------------------------------------------------------

function stats(samples::Vector{UInt64})
    sorted = sort(samples)
    n = length(sorted)
    mn = sorted[1]
    med = sorted[cld(n, 2)]
    mean = sum(Float64.(sorted)) / n
    var = sum((Float64.(sorted) .- mean) .^ 2) / n
    (mn, med, mean, sqrt(var))
end

function bench_file(fmt, path::AbstractString, iterations::Int, warmup::Int)
    local src
    try
        src = read(path, String)
    catch e
        return Dict("path" => path, "ok" => false, "error" => "read: $(sprint(showerror, e))")
    end
    bytes = ncodeunits(src)

    # Sanity gate: the file counts only if the tool formats it without error.
    try
        for _ in 1:max(warmup, 1)
            fmt(src)
        end
    catch e
        return Dict(
            "path" => path, "bytes" => bytes, "ok" => false,
            "error" => "format: $(sprint(showerror, e))",
        )
    end

    samples = Vector{UInt64}(undef, iterations)
    for i in 1:iterations
        GC.gc()
        t0 = time_ns()
        out = fmt(src)
        t1 = time_ns()
        samples[i] = t1 - t0
    end

    mn, med, mean, sd = stats(samples)
    Dict(
        "path" => path, "bytes" => bytes, "ok" => true,
        "min_ns" => mn, "median_ns" => med, "mean_ns" => mean, "stddev_ns" => sd,
    )
end

function run_tool(name, available, fmt, version, files, iterations, warmup)
    if !available
        return Dict("tool" => name, "available" => false, "version" => nothing, "files" => [])
    end
    results = [bench_file(fmt, f, iterations, warmup) for f in files]
    Dict("tool" => name, "available" => true, "version" => string(version), "files" => results)
end

# --- tool loading (top level, so imports settle before `main` dispatches into
# them; runtime `import` inside a function trips Julia 1.12 world-age checks) --

HAVE_RUNIC = false
try
    @eval import Runic
    global HAVE_RUNIC = true
catch e
    @warn "Runic unavailable" exception = e
end

HAVE_JLFMT = false
try
    @eval import JuliaFormatter
    global HAVE_JLFMT = true
catch e
    @warn "JuliaFormatter unavailable" exception = e
end

# --- main --------------------------------------------------------------------

function main()
    filelist = ARGS[1]
    iterations = length(ARGS) >= 2 ? parse(Int, ARGS[2]) : 50
    warmup = length(ARGS) >= 3 ? parse(Int, ARGS[3]) : 3
    outpath = length(ARGS) >= 4 ? ARGS[4] : nothing

    files = filter(!isempty, strip.(readlines(filelist)))

    runic_fmt = HAVE_RUNIC ? (s -> Runic.format_string(s)) : identity
    runic_ver = HAVE_RUNIC ? pkgversion(Runic) : nothing
    jlfmt_fmt = HAVE_JLFMT ? (s -> JuliaFormatter.format_text(s)) : identity
    jlfmt_ver = HAVE_JLFMT ? pkgversion(JuliaFormatter) : nothing

    tools = [
        run_tool("runic", HAVE_RUNIC, runic_fmt, runic_ver, files, iterations, warmup),
        run_tool("juliaformatter", HAVE_JLFMT, jlfmt_fmt, jlfmt_ver, files, iterations, warmup),
    ]

    report = Dict(
        "julia_version" => string(VERSION),
        "iterations" => iterations,
        "warmup" => warmup,
        "tools" => tools,
    )

    json = to_json(report)
    if outpath === nothing
        println(json)
    else
        write(outpath, json)
    end
end

main()
