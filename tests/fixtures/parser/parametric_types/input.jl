Vector{T}
Dict{String, Int}
Array{<:Real}
m = Matrix{Tuple{Int, Int}}
foo(x::Vector{T}) where T = x
bar(x::T) where {T<:Number} = x
Foo{T} where T >: Int
