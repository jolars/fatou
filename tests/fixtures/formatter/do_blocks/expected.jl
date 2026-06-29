map(xs) do x
    x + 1
end

map(xs) do x, y
    y = x + 1
    y * 2
end

g(a, b) do (x, y)
    x
end

foo() do
    return 1
end

open("f") do io
    read(io)
end

reduce(xs) do acc, x

    acc + x
end
