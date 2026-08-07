#!/usr/bin/env julia

# Generate `src/lsp/latex_symbols.rs`: the LaTeX and emoji input sequences the
# Julia REPL substitutes on tab, and the characters they expand to.
#
# `REPL.REPLCompletions.latex_symbols` and `.emoji_symbols` (built from
# `stdlib/REPL/src/latex_symbols.jl` and `emoji_symbols.jl`) are the oracle, so
# the running Julia decides the table and Fatou stays in step with whatever the
# user's REPL does. Run from the repo root:
#
#     julia --startup-file=no scripts/generate-latex-symbols.jl
#
# Regenerate on a Julia bump and commit the result; do not hand-edit
# `latex_symbols.rs`.

import REPL

# Rust string-literal escaping. Most expansions are a printable glyph and read
# best written as themselves, but a combining mark would stack onto the quote
# and an invisible character would vanish entirely (`clippy` rejects those on
# sight), so anything that does not stand on its own becomes a `\u{...}` escape.
function needs_escape(c::Char)
    cat = Base.Unicode.category_abbrev(c)
    !isprint(c) || startswith(cat, "M") || cat in ("Cf", "Cc", "Co", "Cs", "Zs", "Zl", "Zp")
end

function rust_str(s::AbstractString)
    io = IOBuffer()
    print(io, '"')
    for c in s
        if c == '\\'
            print(io, "\\\\")
        elseif c == '"'
            print(io, "\\\"")
        elseif needs_escape(c)
            print(io, "\\u{", uppercase(string(UInt32(c); base = 16)), "}")
        else
            print(io, c)
        end
    end
    print(io, '"')
    String(take!(io))
end

function emit(io, name, doc, table)
    println(io, "/// $doc")
    println(io, "#[rustfmt::skip]")
    println(io, "pub(super) static $name: &[(&str, &str)] = &[")
    for k in sort!(collect(keys(table)))
        println(io, "    (", rust_str(k), ", ", rust_str(table[k]), "),")
    end
    println(io, "];")
end

const latex = REPL.REPLCompletions.latex_symbols
const emoji = REPL.REPLCompletions.emoji_symbols

open("src/lsp/latex_symbols.rs", "w") do io
    println(io, "//! The LaTeX and emoji input sequences the Julia REPL substitutes on tab.")
    println(io, "//!")
    println(io, "//! Generated from `REPL.REPLCompletions` by")
    println(io, "//! `scripts/generate-latex-symbols.jl` (Julia $(VERSION)); regenerate on a")
    println(io, "//! Julia bump and do not hand-edit. Both tables are sorted by key, which")
    println(io, "//! [`super::completion`] relies on to take a prefix range by binary search.")
    println(io)
    emit(io, "LATEX_SYMBOLS",
        "LaTeX sequences (`\\alpha` => `α`), sorted by key.", latex)
    println(io)
    emit(io, "EMOJI_SYMBOLS",
        "Emoji sequences (`\\:smile:` => `😄`), sorted by key.", emoji)
end

println("wrote src/lsp/latex_symbols.rs (latex=", length(latex),
    ", emoji=", length(emoji), ")")
