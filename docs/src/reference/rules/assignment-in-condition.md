# `assignment-in-condition`

Flag a bare `=` assignment used as the test of an `if`/`elseif`/`while`. It is valid Julia but almost always a typo for `==`, so it is reported with a safe fix that rewrites `=` to `==`.

`=` where `==` was meant:

```julia
if x = 5
    println(x)
end
```

```text
example.jl:1:4: warning[assignment-in-condition] assignment used as a condition; did you mean `==`?
```
