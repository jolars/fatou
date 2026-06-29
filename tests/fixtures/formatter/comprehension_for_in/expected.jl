a = [i for i = 1:10]
b = [i for i = 1:10]
c = [i for i ∈ 1:10]
d = [i * j for i = 1:3, j = 1:3]
e = (i for i = 1:5)
f = [i for i = 1:10 if i > 0]
g = [i + j for i = 1:2 for j = 1:2]
h = [i for i in a, j in b]
k = Dict(v => i for (v, i) in pairs)
