expected_version, expected_commit = ARGS
@assert string(VERSION) == expected_version
@assert Base.GIT_VERSION_INFO.commit == expected_commit

# Precompilation starts another interpreter through Base.julia_cmd(), so checking
# a package load also verifies that child processes use the patched ELF loader.
mktempdir() do project
    mkpath(joinpath(project, "src"))
    write(joinpath(project, "Project.toml"), """
    name = "DevenvJuliaProbe"
    uuid = "51a98403-3357-44f4-940d-cefcfca7edeb"
    version = "0.1.0"
    """)
    write(joinpath(project, "src", "DevenvJuliaProbe.jl"), """
    module DevenvJuliaProbe
        const VALUE = 42
    end
    """)
    mktempdir() do depot
        command = `$(Base.julia_cmd()) --startup-file=no --project=$project -e 'using DevenvJuliaProbe; @assert DevenvJuliaProbe.VALUE == 42'`
        run(addenv(command, "JULIA_DEPOT_PATH" => depot, "JULIA_LOAD_PATH" => "@:@stdlib"))
    end
end
