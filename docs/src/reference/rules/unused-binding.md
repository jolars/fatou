# `unused-binding`

Flag a local variable that is assigned but never read in the same scope. Parameters, loop and `catch` variables, struct fields, type parameters, and top-level definitions are exempt, since those are meaningful even when unread. Names beginning with `_` are skipped, following Julia's throwaway convention.

`tmp` is assigned inside `f` but never used:

```julia
function f(x)
    tmp = x + 1
    return x
end
```

```text
example.jl:2:5: warning[unused-binding] local variable `tmp` is assigned but never used
```
