# `discouraged-function`

Flag a call to a function on a configurable deny-list. The built-in set covers Base functions with process-wide or memory-unsafe effects — `exit`, `cd`, `redirect_stdout`, `redirect_stderr`, and the `unsafe_*`/pointer conversions — each reported with the alternative to reach for.

Configure it under `[lint.rules.discouraged-function]`: `functions` replaces the built-in set, `extend-functions` adds to it (an entry there also rewords a built-in), and `functions = {}` silences the rule without ignoring it. Both are tables mapping a function name to the suggestion shown in the diagnostic.

A call carrying a `do` block is never reported, since for `cd` and the `redirect_*` functions that form is the recommended alternative. A qualified callee (`Base.exit`) is a different name and does not match. A built-in name is only reported once it is confirmed to be Base's, so a local of the same name — or a file whose imports cannot be resolved — reports nothing; a name the project configured is reported unless a definition in the same file shadows it. No fix is offered, since the rewrite is a judgment call.

`exit` ends the process and `cd` leaves the working directory changed:

```julia
function cleanup()
    cd("/tmp")
    exit(1)
end
```

```text
warning: discouraged-function
 --> example.jl:2:5
  |
2 |     cd("/tmp")
  |     ^^ `cd` is discouraged: use the `cd(f, dir)` do-block form so the working directory is restored
warning: discouraged-function
 --> example.jl:3:5
  |
3 |     exit(1)
  |     ^^^^ `exit` is discouraged: let the caller decide when the process ends
```
