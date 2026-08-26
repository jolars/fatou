a = 1         # first
long_name = 2 # second

blank_a = 1 # first

blank_long_name = 2 # second

plain_a = 1 # first
plain_middle = 2
plain_long_name = 3 # second

own_a = 1 # first
# standalone
own_long_name = 2 # second

block_a = 1 # first
block_middle = 2 #= block =#
block_long_name = 3 # second

function f()
    x = 1           # first
    longer_name = 2 # second
end

f(
    a,         # first
    long_name, # second
)

A = [
    1 2   # first
    10 20 # second
]

begin
    x = 1
end             # first
longer_name = 2 # second
