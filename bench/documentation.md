# Markdown navigation measurements

`documentation.json` records the before/after measurements for caching the
file's Markdown anchor and definition index. These are local measurements, not
CI gates. The baseline is recorded in the artifact.

The index depends on decoded static payloads in source order. Source maps stay
in the current semantic model, so Julia body edits, indentation changes, and
equivalent escape spellings reuse the index while navigation follows the live
buffer. A docstring content edit rebuilds the index on its next request.

## LSP requests

Build and save each revision's release CLI, then alternate sessions:

```sh
python3 bench/lsp_documentation.py \
  --before 'taskset -c 2 /tmp/fatou-doc-before lsp' \
  --server 'taskset -c 2 /tmp/fatou-doc-after lsp' \
  --rounds 3 --out bench/documentation-synthetic.tmp.json
```

Add `--source bench/corpus/JuliaSyntax/src/parser.jl` for the real-file case.
Otherwise the harness generates 1, 100, and 500 documented functions. Both
routes append two probe docstrings, placing definition targets at the end of the
file. Sessions open a loose file without package harvesting. Diagnostics confirm
that analysis has consumed the buffer before requests start.

Three warmups precede each group of 30 measured requests. The harness checks
stable answers and compares response hashes across revisions, normalizing only
the temporary file URI. Samples include stdio transport, scheduling, and JSON
serialization. On small inputs these can hide the handler's improvement.

## Direct handlers

The ignored `lsp::documentation_bench::warm_markdown_requests` test measures the
same cached LSP handlers without transport or scheduling. To compare an older
revision, copy this test module and its `#[cfg(test)]` declaration in
`src/lsp.rs` into a detached worktree of that revision. Build each with
`cargo test --release --lib --no-run`, saving the reported test executable.

Write a benchmark source using `fixture(n) + PROBES` from
`bench/lsp_documentation.py`, or append `PROBES` to the real file. Then run:

```sh
FATOU_DOC_BENCH_SOURCE=/tmp/fatou-doc-500.jl \
FATOU_DOC_BENCH_OUTPUT=/tmp/handler-before.json \
  taskset -c 2 /tmp/fatou-doc-handler-before \
  lsp::documentation_bench::warm_markdown_requests --ignored --exact
```

Each execution records 20 samples of 20 calls, after warming analysis and the
requested feature. `FATOU_DOC_BENCH_ITERATIONS` changes the calls per sample.
Alternate before/after executables to limit drift, and retain minima alongside
medians. Neither harness includes initial Julia analysis or index construction
in its warm timings. The cursor's own docstring is still parsed per request.
