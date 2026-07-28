dottable(x::Expr) = x.head !== :$
dottable(x::Symbol) = x !== :$

function showerror(io::IO, ex::KeyError)
    if ex.func === :var"dict key"
        print(io, "key not found")
    elseif ex.func === :setindex!
        print(io, "index out of range")
    end
end

const heads = [:$, :., :var"hygienic-scope", :escape]
isexpr(e, :$) && return QuoteNode(e)
Expr(:call, :var"@inline", :$)
