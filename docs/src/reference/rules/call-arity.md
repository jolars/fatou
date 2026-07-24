# `call-arity`

Flag a call that no visible method of the function accepts: a positional count outside every method's range, or a keyword argument no positionally-matching method declares. Such a call raises `MethodError` at runtime. The method table unions every tier name resolution sees — the file's own definitions, the workspace package, and the harvested library, qualified extensions included — so an unknown method always silences the check rather than triggering it. Calls that splat arguments, carry `do` blocks, sit in macro calls or quoted code, or target constructors and callable values are exempt, and a file that `eval`s or `include`s outside a known workspace is skipped entirely. Off by default: the rule needs project context to be sound, so the language server enables it for workspace member files, while the CLI leaves it opt-in for self-contained scripts.

`half` has no two-argument method:

```julia
half(x) = x / 2

half(3, 4)
```

```text
warning: call-arity
 --> example.jl:3:1
  |
3 | half(3, 4)
  | ^^^^^^^^^^ no method of `half` takes 2 positional arguments (methods accept 1)
```
