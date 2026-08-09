#!/usr/bin/env julia
# Run LanguageServer.jl over stdio the way its editor clients do: hold on to the
# real stdout for the JSON-RPC stream and send everything else to stderr, so
# package chatter cannot corrupt the protocol.
#
# Usage: julia --startup-file=no --project=<lsenv/languageserver> ls_runner.jl <workspace>

using LanguageServer

const conn_in = stdin
const conn_out = stdout
redirect_stdout(stderr)

workspace = length(ARGS) >= 1 ? ARGS[1] : ""
depot = get(ENV, "JULIA_DEPOT_PATH", joinpath(homedir(), ".julia"))

run(LanguageServerInstance(conn_in, conn_out, workspace, depot))
