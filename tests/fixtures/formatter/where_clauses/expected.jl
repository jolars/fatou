f(x) where {T}
g(x)::T where {T <: Real}
h(x) where {T}
k(x) where {T, S}
m(x) where {T} where {S}
Tuple{T} where {T}
Array{T, N} where {T, N}
foo(x::S, y::T) where {S, T}
bar(x) where {T >: Int}
