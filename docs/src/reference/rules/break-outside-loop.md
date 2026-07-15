# `break-outside-loop`

Flag a `break` or `continue` with no enclosing `for` or `while` loop. The code parses but always fails at lowering with "break or continue outside loop" — including inside a closure, do-block, or comprehension body defined within a loop, since `break` cannot cross a function boundary.

`break` with no loop in sight:

```julia
function process(x)
    if x < 0
        break
    end
    x
end
```

```text
error: break-outside-loop
 --> example.jl:3:9
  |
3 |         break
  |         ^^^^^ `break` outside of a `for` or `while` loop
```

A do-block body is an anonymous function, so the outer loop is out of reach:

```julia
for i in 1:3
    foreach(1:2) do x
        continue
    end
end
```

```text
error: break-outside-loop
 --> example.jl:3:9
  |
3 |         continue
  |         ^^^^^^^^ `continue` outside of a `for` or `while` loop
```
