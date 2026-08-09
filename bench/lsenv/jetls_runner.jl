#!/usr/bin/env julia
# Run JETLS over stdio, under the same stdout discipline as ls_runner.jl.
#
# Usage: julia --startup-file=no --threads=auto --project=<lsenv/jetls> jetls_runner.jl
#
# `--threads=auto` is not a benchmark thumb on the scale: it is what JETLS's own
# `[apps.jetls]` julia_flags declare, so it is the configuration a user gets.

using JETLS

const conn_in = stdin
const conn_out = stdout
redirect_stdout(stderr)

exit(JETLS.runserver(conn_in, conn_out))
