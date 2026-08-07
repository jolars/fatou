for i ∈ xs end
for i ∈ 1:3, j ∈ 4:6 end
for i ∈ 1:3, j = 4:6, k in 7:9 end
for (a, b) ∈ xs end
for a::Int ∈ xs end
for i ∈ f(x) end
for i ∈ (1, 2, 3) end
for i ∈ [1, 2] end
for i ∈ xs y += i end
for outer i ∈ xs end
for i ∈ xs, outer j ∈ ys end
[x for i ∈ xs]
(x for i ∈ xs)
[x for i ∈ xs if i > 1]
[x for i ∈ xs for j ∈ ys]
[f(i, j) for i ∈ 1:2, j ∈ 1:3]
Dict(k => v for (k, v) ∈ pairs(d))
sum(x^2 for x ∈ 1:10)
a ∈ b
x = a ∈ b
if a ∈ b c end
filter(x -> x ∈ s, xs)
map(i ∈ s for i ∈ xs)
