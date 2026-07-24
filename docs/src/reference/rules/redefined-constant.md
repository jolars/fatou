# `redefined-constant`

Flag a write that redefines a constant name, or defines over a name that already holds a value: reassigning a `const` binding, assigning to a global function, type, or module name (those bind implicit constants), defining a function over a plain value, or declaring a value `const` after the fact. All of these error at runtime when both sites execute. A definition and a write in disjoint branches of the same `if` are exempt — only one branch runs. Adding a method to a function and defining an outer constructor on a type are legal and stay silent. No fix: the rule cannot know which of the two definitions the author meant to keep.

Reassigning a `const` binding errors at runtime:

```julia
const threshold = 1.0
threshold = 2.0
```

```text
warning: redefined-constant
 --> example.jl:2:1
  |
2 | threshold = 2.0
  | ^^^^^^^^^ reassignment of constant `threshold`
```

`count` already holds a value, so the method definition fails:

```julia
count = 0
count(xs) = length(xs)
```

```text
warning: redefined-constant
 --> example.jl:2:1
  |
2 | count(xs) = length(xs)
  | ^^^^^ cannot define function `count`: it already has a value
```
