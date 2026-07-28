a #=c=# + b
x = a #=c=# + b
a #=c=# = 1
f(a #=::Union{Nothing,T}=# = nothing)
g(x #=c=# = 2, y = 3)
@test #=T=# has_thrown_escape(result.state[Argument(2)], t)
@newinterp DebugInterp #=ephemeral_cache=#true
struct A #=c=# <: B end
a #=
multi
line
=# + b
