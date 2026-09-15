#!/usr/bin/env julia

# Regenerate the Unicode operator table from the pinned JuliaSyntax package.
# Run from the project environment on a Julia or JuliaSyntax version bump:
#
#     julia --startup-file=no scripts/generate-unicode-ops.jl

using JuliaSyntax

const TIERS = [
    JuliaSyntax.is_prec_assignment => "UniAssign",
    JuliaSyntax.is_prec_arrow => "UniArrow",
    JuliaSyntax.is_prec_comparison => "UniComparison",
    JuliaSyntax.is_prec_colon => "UniColon",
    JuliaSyntax.is_prec_plus => "UniPlus",
    JuliaSyntax.is_prec_times => "UniTimes",
    JuliaSyntax.is_prec_power => "UniPower",
]

function operator_tier(kind)
    for (predicate, tier) in TIERS
        predicate(kind) && return tier
    end
    # These prefix-only operators have no infix precedence tier.
    string(kind) in ("¬", "√", "∛", "∜") && return "UniRadical"
    error("unmapped Unicode operator: $kind")
end

operators = Tuple{Char,String}[]
for kind in JuliaSyntax.Tokenize._nondot_symbolic_operator_kinds()
    spelling = string(kind)
    length(spelling) == 1 || continue
    char = only(spelling)
    isascii(char) && continue
    push!(operators, (char, operator_tier(kind)))
end
sort!(operators; by = first)

const OUTPUT = joinpath(
    @__DIR__, "..", "crates", "fatou-parser", "src", "parser", "unicode_ops.rs")

open(OUTPUT, "w") do io
    println(io, "//! Single-codepoint Unicode operators and their precedence tiers.")
    println(io, "//!")
    println(io, "//! Generated from JuliaSyntax's operator-kind tables ",
        "(julia_version=", VERSION, " juliasyntax_version=", pkgversion(JuliaSyntax), ").")
    println(io, "//! Every entry is a length-1 non-ASCII operator string from")
    println(io, "//! `Tokenize._nondot_symbolic_operator_kinds()`, classified by the")
    println(io, "//! `is_prec_*` predicate for its kind. The table is sorted by code point so")
    println(io, "//! [`unicode_op_kind`] can binary-search it. Regenerate with")
    println(io, "//! `scripts/generate-unicode-ops.jl` on a Julia or JuliaSyntax bump;")
    println(io, "//! do not hand-edit.")
    print(io, """

    use super::lexer::TokKind;

    /// Maps a Unicode operator code point to the [`TokKind`] for its precedence
    /// tier, or `None` when the char is not a Julia operator.
    pub(super) fn unicode_op_kind(c: char) -> Option<TokKind> {
        UNICODE_OPS
            .binary_search_by(|&(k, _)| k.cmp(&c))
            .ok()
            .map(|i| UNICODE_OPS[i].1)
    }

    /// `(operator char, tier kind)`, sorted ascending by code point.
    #[rustfmt::skip]
    static UNICODE_OPS: &[(char, TokKind)] = &[
    """)
    for (char, tier) in operators
        println(io, "    ('\\u{", string(UInt32(char); base = 16),
            "}', TokKind::", tier, "), // ", char)
    end
    println(io, "];")
end

println("wrote $OUTPUT (operators=", length(operators), ")")
