@recipe EunoiaDiagram (fit,) begin
    colors = Makie.automatic
end
@foo f (x)
@foo a [1]
@foo A {T}
@foo a +b
@foo x :y
@foo a :b c
@foo (f)(x)
@foo g(x) [1]
f(@m a (b))
@jl_assert !is_leaf(st) (st, "msg")
@m -a(b) (c, d)
@m !a (b)
