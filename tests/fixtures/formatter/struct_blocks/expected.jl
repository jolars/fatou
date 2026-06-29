struct Foo
    x::Int
    y::Any
end

mutable struct Bar
    a::Any
    b::Any
end

struct Point{T}
    x::T
    y::T
end

struct Dog<:Animal
    name::Any
end

struct Pair
    x::Any;
    y::Any
end

struct Empty end

struct Spaced
    x::Any


    y::Any
end

struct WithCtor
    val::Any
    WithCtor() = new(0)
end
