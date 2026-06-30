struct Foo
    x::Int
    y
end

mutable struct Bar
    a
    b
end

struct Point{T}
    x::T
    y::T
end

struct Dog <: Animal
    name
end

struct Pair
    x
    y
end

struct Empty end

struct Newline end

mutable struct EmptyMut end

struct EmptySuper <: Super end

struct Spaced
    x

    y
end

struct WithCtor
    val
    WithCtor() = new(0)
end
