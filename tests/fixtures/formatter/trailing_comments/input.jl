begin
    x = 1 # note
    z=3# tight
    y = 2; w = 4 # after two
end
for i in 1:3
    f(i) # loop comment
end
while c
    g() # while body
end
let a = 1
    b = a # in let
end
if c
    p = 1 # then
elseif d
    q = 2 # elseif
else
    # own line first
    r = 3 # else
end
try
    s() # try
catch e
    h(e) # catch
finally
    cleanup() # finally
end
