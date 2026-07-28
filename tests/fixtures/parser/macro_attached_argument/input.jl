issue49823_events = @NamedTuple{evid::Int8, base_time::Float64}[
    (evid = 1, base_time = 0.0), (evid = -1, base_time = 0.0)]
nt = @NamedTuple{a::Int}
x = @SVector[1, 2]
y = @views A[1:2]
z = @m {a}
w = @eval(expr)
v = @m(a, b)[1]
