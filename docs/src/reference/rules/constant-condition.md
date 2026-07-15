# `constant-condition`

Flag a `true`/`false` literal used as an `if`/`elseif`/`while` test or as an operand of the short-circuit `&&`/`||`. The branch or short-circuit is decided before the code runs, so the literal is usually leftover debugging or a mistyped name. `while true` is exempt as Julia's idiomatic infinite loop. No fix: removing the constant means restructuring the branch.

A literal `if` test always takes the branch:

```julia
if true
    println("always")
end
```

```text
warning: constant-condition
 --> example.jl:1:4
  |
1 | if true
  |    ^^^^ this condition is always `true`
```

A literal operand decides `&&` at parse time:

```julia
ok = false && check(x)
```

```text
warning: constant-condition
 --> example.jl:1:6
  |
1 | ok = false && check(x)
  |      ^^^^^ `&&` has a constant `false` operand
```
