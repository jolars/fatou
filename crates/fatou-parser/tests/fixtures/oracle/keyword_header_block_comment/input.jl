let #= c =# x = 1, #= d =# y = 2
    x + y
end

while #= c =# cond
    step!()
end

if #= c =# cond
    a
elseif #= d =# b
    c
end

for #= c =# i in 1:3
    a
end

struct #= c =# Foo
end

function #= c =# f()
end

macro #= c =# m()
end

module #= c =# M
end

f() do #= c =# x, y
    x
end

try
    a
catch #= c =# e
    b
end

const #= c =# k = 1
