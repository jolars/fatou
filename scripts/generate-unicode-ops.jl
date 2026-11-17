#!/usr/bin/env julia

# Regenerate the Unicode operator table with the locked oracle toolchain:
#
#     bash scripts/refresh-juliasyntax-oracle.sh

include("juliasyntax-oracle.jl")

const TIERS = [
    JuliaSyntax.is_prec_assignment => "UniAssign",
    JuliaSyntax.is_prec_arrow => "UniArrow",
    JuliaSyntax.is_prec_comparison => "UniComparison",
    JuliaSyntax.is_prec_colon => "UniColon",
    JuliaSyntax.is_prec_plus => "UniPlus",
    JuliaSyntax.is_prec_times => "UniTimes",
    JuliaSyntax.is_prec_power => "UniPower",
]

function operator_tier(token, char)
    for (predicate, tier) in TIERS
        predicate(token) && return tier
    end
    # These prefix-only operators have no infix precedence tier.
    char in ('¬', '√', '∛', '∜') && return "UniRadical"
    error("unmapped Unicode operator: $char")
end

operators = Tuple{Char,String}[]
for codepoint in 0x80:0x10ffff
    char = Char(codepoint)
    isvalid(char) && JuliaSyntax.Tokenize.is_operator_start_char(char) || continue
    # JuliaSyntax 2 stores generic operators' precedence on tokens, not kinds.
    # Asking the lexer also includes operators with dedicated token kinds.
    tokens = collect(JuliaSyntax.Tokenize.tokenize(string(char)))
    length(tokens) == 2 && JuliaSyntax.kind(last(tokens)) == JuliaSyntax.K"EndMarker" ||
        error("expected one operator token and EOF: $char")
    raw = first(tokens)
    head = JuliaSyntax.SyntaxHead(
        JuliaSyntax.kind(raw), JuliaSyntax.set_numeric_flags(Int(raw.op_precedence)),
    )
    token = JuliaSyntax.Token(head, UInt32(1):UInt32(ncodeunits(string(char))))
    JuliaSyntax.is_operator(token) || error("expected an operator: $char")
    push!(operators, (char, operator_tier(token, char)))
end
sort!(operators; by = first)
isempty(operators) && error("JuliaSyntax produced no Unicode operators")

const OUTPUT = joinpath(
    @__DIR__, "..", "crates", "fatou-parser", "src", "parser", "unicode_ops.rs")

open(OUTPUT, "w") do io
    println(io, "//! Single-codepoint Unicode operators and their precedence tiers.")
    println(io, "//!")
    println(io, "//! Generated from JuliaSyntax's lexer ",
        "(julia_version=", VERSION, " juliasyntax_version=", pkgversion(JuliaSyntax), ").")
    if haskey(ENV, "FATOU_ORACLE_REVISION")
        println(io, "//! Julia and JuliaSyntax commit: ", ENV["FATOU_ORACLE_REVISION"], ".")
    end
    println(io, "//! Every entry is a non-ASCII operator start character, classified by the")
    println(io, "//! `is_prec_*` predicates on its token. The table is sorted by code point so")
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
