p = :(
    if a
        b
    end
)
q = :(
    function f(x)
        x
    end
)
r = :(
    let x = 1
        x
    end
)
s = :(
    begin
        x
    end
)
t = :(
    for i in x
        g(i)
    end
)
u = :(
    quote
        y
    end
)
v = :(a; b; c)
w = :(
    if a
        b
        c
    end
)
