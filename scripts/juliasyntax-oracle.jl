# Keep the grammar version independent of the interpreter running the oracle.
using JuliaSyntax

if haskey(ENV, "FATOU_ORACLE_REVISION") &&
        Base.GIT_VERSION_INFO.commit != ENV["FATOU_ORACLE_REVISION"]
    error("The oracle must use Julia and JuliaSyntax from the same locked commit")
end

const ORACLE_SYNTAX_VERSION = let
    version = VersionNumber(get(ENV, "FATOU_ORACLE_SYNTAX_VERSION", string(VERSION)))
    VersionNumber(version.major, version.minor)
end

function render_oracle(src::AbstractString; version = ORACLE_SYNTAX_VERSION)
    node = JuliaSyntax.parseall(
        JuliaSyntax.SyntaxNode, src;
        version, ignore_errors = true,
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
            println(io, "julia_binary_source=", get(ENV, "FATOU_ORACLE_BINARY_SOURCE", "environment"))
            if haskey(ENV, "FATOU_ORACLE_ARCHIVE_SHA256")
                println(io, "julia_archive_sha256=", ENV["FATOU_ORACLE_ARCHIVE_SHA256"])
            end
        end
    end
end
