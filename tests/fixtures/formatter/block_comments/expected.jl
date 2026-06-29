begin
    # leading own-line comment
    x = 1
    # interior comment
    y = 2
end

let a = 1
    # comment in let
    b = a + 1
end

for i = 1:3
    # loop body comment
    f(i)
end

while c
    # while body comment
    step()
end

if cond
    # then branch
    a = 1
else
    # else branch
    b = 2
end

try
    # try body
    risky()
catch e
    # catch body
    handle(e)
end

begin
    # outer
    if inner
        # nested branch
        g()
    end
    # after nested
    z = 2
end
