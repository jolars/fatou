# Keep the grammar version independent of the interpreter running the oracle.
using JuliaSyntax

const ORACLE_SYNTAX_VERSION = let
    version = VersionNumber(get(ENV, "FATOU_ORACLE_SYNTAX_VERSION", string(VERSION)))
    VersionNumber(version.major, version.minor)
end

function render_oracle(src::AbstractString)
    node = JuliaSyntax.parseall(
        JuliaSyntax.SyntaxNode, src;
        version = ORACLE_SYNTAX_VERSION, ignore_errors = true,
    )
    sprint(print, node)
end

function write_oracle_source(path)
    open(path, "w") do io
        println(io, "julia_version=", VERSION)
        println(io, "julia_commit=", Base.GIT_VERSION_INFO.commit)
        println(io, "juliasyntax_version=", pkgversion(JuliaSyntax))
        println(io, "syntax_version=", ORACLE_SYNTAX_VERSION)
        if haskey(ENV, "FATOU_ORACLE_REVISION")
            println(io, "juliasyntax_repository=https://github.com/JuliaLang/julia")
            println(io, "juliasyntax_revision=", ENV["FATOU_ORACLE_REVISION"])
        end
    end
end
