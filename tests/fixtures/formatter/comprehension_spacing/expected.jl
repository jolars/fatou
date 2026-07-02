a = [x for x in v if x > 0]
b = Int[x for x in v]
c = {k => v for k in ks}
d = (i for i in 1:5)
e = [i for i in 1:2 for j in 1:2]
f = Dict(v => i for (v, i) in pairs)
