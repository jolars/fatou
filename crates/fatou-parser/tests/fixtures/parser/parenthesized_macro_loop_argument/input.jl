f(n) = (@allocated for _ in 1:n
    sin(1.0)
end) ÷ n
(@elapsed for i in xs
    f(i)
end)
(@outer Mod.@inner #= loop =# for x in xs, y in ys
    f(x, y)
end)
(@m label for i in xs
    f(i)
end)
(x, @m for i in xs
    f(i)
end)
(; x = @m for i in xs
    f(i)
end)
:(@m for i in xs
    f(i)
end)
(@m (x for x in xs))
[@m f(x) for x in xs]
f(@outer @inner x for x in xs)
f((@m for i in xs))
[(@m for i in xs)]
f(x, (@m for i in xs))
T{(@m for i in xs)}
