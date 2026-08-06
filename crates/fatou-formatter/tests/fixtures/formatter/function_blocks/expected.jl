function f(x)
    return x + 1
end
function g(x)
    y = x * 2
    return y
end
function h(x)
    return x
end
g = function (x)
    return x * 2
end
function k(x)::Int where {T}

    return x
end
macro m(x)
    return esc(x)
end
function empty() end
function empty2() end
function empty3() end
macro noop() end
function add(a, b)
    c = a + b
    c
end
