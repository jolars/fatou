# `type-piracy`

Flag a method definition that extends a function the current module does not own, using only argument types it does not own either ("type piracy"). Because Julia dispatches on one global method table, such a method silently changes behavior for every other user of those types the moment the module loads. A definition is fine as long as it owns the function or at least one argument type (a type parameter or `where` bound counts). The rule is sound-first: it flags only when it can prove the function and every readable argument type are foreign, withholding on anything unknown, and it skips the whole file when a whole-module `using` cannot be resolved. Off by default: it needs project context, so the language server enables it for workspace member files while the CLI leaves it opt-in via `--select`.

Extending `Base.show` for only Base types is piracy:

```julia
Base.show(io::IO, x::Int) = print(io, x)
```

```text
warning: type-piracy
 --> example.jl:1:6
  |
1 | Base.show(io::IO, x::Int) = print(io, x)
  |      ^^^^ `Base.show` commits type piracy: it extends a function this module does not own, and no argument type is owned here either
```
