@inbounds @simd for gix in gd.groups
    x = gix
end
@outer Mod.@inner for x in 1:2, y in ys
    x + y
end
[@outer @inner f(x) for x in xs]
f(@outer @inner x for x in xs)
