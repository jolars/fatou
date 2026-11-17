(@info "message"
    x=1
    y=2)

(
    # leading comment
    Base.@info "message" # argument comment
    x=1

    y=2
)

(A.B.@m a
    b)
(@A.B.m a
    b)
(@var"#" a
    b)
(@(A) a
    b)
(f(x).@m a
    b)
($A.@m a
    b)
(@m(a).@n b
    c)

(@m @n x y
    z)
(@m (@n x
        y)
    z)
(@m x, @n a
    b)
(@m x + @n a
    b)

(@m begin
    @n x
    y
end
    z)
(@m function f()
    @n x
    y
end
    z)
(@m x
    begin
        @n a
        b
    end)

(@m x
    [1]
    "s"
    'c'
    @n y)
(@m +
    x)
(@m x
    y, z)
(@m x
    y; z)
(@m a
    , b)
((@m x
    y), z)
f((@m x
    y))
[(@m x
    y)]
(@m x y
    for y in ys)
(@m
    for i in xs)
(@m for i in xs end)

:(@m x
    y)
$(@m x
    y)
"$((@m x
    y))"
(@doc x

    y)

(@m x
    - y)
(@m x
    where T)
(@m x
    ::T)

((A).@m x
    y)
(@m x
    y, z; w)
2@m x
