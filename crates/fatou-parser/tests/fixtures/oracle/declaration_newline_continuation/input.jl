const

x = 1
const # declaration
# continuation
c = 2
const #= block
comment =#
d::Int = 3
const global
#= between =#
g = 4
global const
h = 5
global
x, y
local
z = 6
function f()
    local # declaration
    a, b = 1, 2
    return
    a = 3
end
mutable struct S
    const
    field::Int
end
const last = 7
next = 8
