#!/usr/bin/env julia

# Generate `crates/fatou-parser/src/parser/unicode_ident.rs`: the code-point ranges Julia accepts as
# identifier start and continuation characters.
#
# JuliaSyntax's tokenizer classifies identifier characters with
# `Base.is_id_start_char` / `Base.is_id_char` (see `is_identifier_start_char` /
# `is_identifier_char` in JuliaSyntax's `tokenize.jl`), so those predicates are
# the oracle. ASCII is handled directly in the Rust lexer; only the non-ASCII
# ranges are emitted here. Run from the repo root:
#
#     julia --startup-file=no scripts/generate-unicode-ident.jl
#
# Regenerate on a Julia/JuliaSyntax bump and commit the result; do not hand-edit
# `unicode_ident.rs`.

function ranges(pred)
    rs = Tuple{UInt32,UInt32}[]
    inr = false
    s = UInt32(0)
    for c in 0x80:0x10FFFF
        ch = Char(c)
        ok = isvalid(ch) && pred(ch)
        if ok && !inr
            inr = true
            s = UInt32(c)
        elseif !ok && inr
            inr = false
            push!(rs, (s, UInt32(c - 1)))
        end
    end
    inr && push!(rs, (s, UInt32(0x10FFFF)))
    rs
end

function juliasyntax_version()
    for (uuid, dep) in Base.loaded_modules
        dep === Base && continue
        if uuid.name == "JuliaSyntax"
            return string(pkgversion(dep))
        end
    end
    try
        m = Base.require(Base.PkgId(
            Base.UUID("70703baa-626e-46a2-a12c-08ffd08c73b4"), "JuliaSyntax"))
        return string(pkgversion(m))
    catch
        return "unknown"
    end
end

function emit(io, name, doc, rs)
    println(io, "/// $doc")
    println(io, "#[rustfmt::skip]")
    println(io, "static $name: &[(u32, u32)] = &[")
    for (a, b) in rs
        println(io, "    (0x", string(a; base = 16), ", 0x", string(b; base = 16), "),")
    end
    println(io, "];")
end

start = ranges(Base.is_id_start_char)
cont = ranges(Base.is_id_char)
jsver = juliasyntax_version()

open(joinpath(@__DIR__, "..", "crates", "fatou-parser", "src", "parser", "unicode_ident.rs"), "w") do io
    println(io, "//! Non-ASCII identifier start and continuation code points.")
    println(io, "//!")
    println(io, "//! Generated from `Base.is_id_start_char` / `Base.is_id_char` ",
        "(julia_version=", VERSION, " juliasyntax_version=", jsver, ").")
    println(io, "//! These are the predicates JuliaSyntax's tokenizer uses to scan")
    println(io, "//! identifiers, so the tables track the oracle exactly. ASCII is handled")
    println(io, "//! inline in the lexer; only non-ASCII ranges live here. Each entry is an")
    println(io, "//! inclusive `(start, end)` code-point range, sorted ascending, so the")
    println(io, "//! lookups can binary-search. Regenerate with")
    println(io, "//! `scripts/generate-unicode-ident.jl` on a Julia/JuliaSyntax bump; do not")
    println(io, "//! hand-edit.")
    println(io)
    println(io, "/// Whether `c` may begin a Julia identifier (non-ASCII only).")
    println(io, "pub(super) fn is_unicode_ident_start(c: char) -> bool {")
    println(io, "    in_ranges(ID_START, c)")
    println(io, "}")
    println(io)
    println(io, "/// Whether `c` may continue a Julia identifier (non-ASCII only).")
    println(io, "pub(super) fn is_unicode_ident_continue(c: char) -> bool {")
    println(io, "    in_ranges(ID_CONTINUE, c)")
    println(io, "}")
    println(io)
    println(io, "fn in_ranges(ranges: &[(u32, u32)], c: char) -> bool {")
    println(io, "    let c = c as u32;")
    println(io, "    ranges")
    println(io, "        .binary_search_by(|&(lo, hi)| {")
    println(io, "            if c < lo {")
    println(io, "                core::cmp::Ordering::Greater")
    println(io, "            } else if c > hi {")
    println(io, "                core::cmp::Ordering::Less")
    println(io, "            } else {")
    println(io, "                core::cmp::Ordering::Equal")
    println(io, "            }")
    println(io, "        })")
    println(io, "        .is_ok()")
    println(io, "}")
    println(io)
    emit(io, "ID_START", "Non-ASCII identifier-start ranges, sorted ascending by code point.", start)
    println(io)
    emit(io, "ID_CONTINUE", "Non-ASCII identifier-continuation ranges, sorted ascending by code point.", cont)
end

println("wrote crates/fatou-parser/src/parser/unicode_ident.rs (start=", length(start), " ranges, cont=", length(cont), " ranges)")
