#!/usr/bin/env julia
#
# Regenerate the pinned semantic projections for Fatou's Julia Markdown oracle.
# The runtime parser never loads Julia; this script is test-data generation only.

using Markdown

const REPO_ROOT = normpath(joinpath(@__DIR__, ".."))
const CORPUS_DIR = joinpath(
    REPO_ROOT,
    "crates",
    "fatou-parser",
    "tests",
    "fixtures",
    "documentation",
    "oracle",
)

hex(text::AbstractString) = bytes2hex(codeunits(text))
atom(name, value::AbstractString) = "(" * name * " " * hex(value) * ")"
items(value) = value isa AbstractVector ? value : Any[value]

function render_many(values)
    join((render(value) for value in items(values)), " ")
end

function render(value)
    if value isa AbstractString
        return atom("text", value)
    elseif value isa Markdown.MD
        return "(document " * render_many(value.content) * ")"
    elseif value isa Markdown.Paragraph
        return "(paragraph " * render_many(value.content) * ")"
    elseif value isa Markdown.Header
        level = first(typeof(value).parameters)
        return "(heading $level " * render_many(value.text) * ")"
    elseif value isa Markdown.Bold
        return "(strong " * render_many(value.text) * ")"
    elseif value isa Markdown.Italic
        return "(emphasis " * render_many(value.text) * ")"
    elseif value isa Markdown.Code
        return "(code $(hex(value.language)) $(hex(value.code)))"
    elseif value isa Markdown.LaTeX
        return atom("math", value.formula)
    elseif value isa Markdown.Link
        return "(link (" * render_many(value.text) * ") $(hex(value.url)))"
    elseif value isa Markdown.Image
        return "(image $(hex(value.alt)) $(hex(value.url)))"
    elseif value isa Markdown.LineBreak
        return "(linebreak)"
    elseif value isa Markdown.Footnote
        if value.text === nothing
            return atom("footnote-ref", value.id)
        end
        return "(footnote-def $(hex(value.id)) " * render_many(value.text) * ")"
    elseif value isa Markdown.BlockQuote
        return "(blockquote " * render_many(value.content) * ")"
    elseif value isa Markdown.Admonition
        return "(admonition $(hex(value.category)) $(hex(value.title)) " *
               render_many(value.content) * ")"
    elseif value isa Markdown.List
        rendered_items = join(
            ("(item " * render_many(item) * ")" for item in value.items),
            " ",
        )
        return "(list $(value.ordered) $(value.loose) $rendered_items)"
    elseif value isa Markdown.Table
        align = join(string.(value.align), ",")
        rendered_rows = join(
            (
                "(row " *
                join(("(cell " * render_many(cell) * ")" for cell in row), " ") *
                ")" for row in value.rows
            ),
            " ",
        )
        return "(table $align $rendered_rows)"
    elseif value isa Markdown.HorizontalRule
        return "(thematic-break)"
    elseif value isa Expr || value isa Symbol
        # Static analysis retains interpolation source but never evaluates it.
        # The oracle checks only that Julia and Fatou agree on its boundary.
        return "(interpolation)"
    end
    error("unsupported Markdown oracle value: $(typeof(value))")
end

function main()
    isdir(CORPUS_DIR) || error("corpus directory not found: $CORPUS_DIR")
    written = 0
    for slug in sort(readdir(CORPUS_DIR))
        directory = joinpath(CORPUS_DIR, slug)
        isdir(directory) || continue
        input = joinpath(directory, "input.md")
        isfile(input) || continue
        expected = render(Markdown.parse(read(input, String)))
        open(joinpath(directory, "expected.sexpr"), "w") do io
            println(io, expected)
        end
        written += 1
    end
    open(joinpath(CORPUS_DIR, ".markdown-source"), "w") do io
        println(io, "julia_version=", VERSION)
        println(io, "stdlib=Markdown")
    end
    println("wrote $written Markdown oracle fixture(s) to $CORPUS_DIR")
end

main()
