struct Point
    x
    y
end

mutable struct Counter
    n
end

struct Wrapper <: AbstractWrapper
    value
end

module M
    f(x) = x
end

baremodule B
end
