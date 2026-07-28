typegroup
    struct TG_Node
        data::Int
        edges::Vector{TG_Edge}
    end
    mutable struct TG_Edge
        from::TG_Node
    end
end
typegroup struct A end end
typegroup
    "a docstring"
    struct D end
end
typegroup
    abstract type X end
    primitive type Y 8 end
end
typegroup
    @generate_types()
end
typegroup = 1
typegroup(x)
x = typegroup
