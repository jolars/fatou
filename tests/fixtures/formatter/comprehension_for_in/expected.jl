a = [i for i in 1:10]
b = [i for i in 1:10]
c = [i for i in 1:10]
d = [i * j for i in 1:3, j in 1:3]
e = (i for i in 1:5)
f = [i for i in 1:10 if i > 0]
g = [i + j for i in 1:2 for j in 1:2]
h = [i for i in a, j in b]
k = Dict(v => i for (v, i) in pairs)
