a[return end]
a[return begin]
a[return begin + 1:end - 1]
a[return end, begin]
a[return return end]
a[return x ? begin : end]
a[x ? return begin : end]
a[x ? return end : begin]
a[x ? return : end]
a[x ? return begin, end : y]
a[return f(begin, end)]
a[return (end)]
a[return (begin)]
a[return begin #= index marker =# + 1]
a[return [begin x end]]
a[return [end]]
a[return begin:2:end]
a[return end for i in xs]
a[x ? return end : begin for i in xs]
x ? return : y
x ? return a : y
x ? return a, b : y
x ? return return a : y
x ? y ? return a : b : z
x ? return y ? a : b : z
x ? return (a:b) : y
x ? return f(a:b) : y
x ? return [a:b] : y
x ? return (:a) : y
x ? return a = b : y
x ? return #= empty =# : y
x ? return a #= value =# : y
x ? return : return a:b
return a:b
[return begin a end]
f(return a, b)
[return x for i in xs]
(x ? return # empty branch
 : y)
(x ? return a # valued branch
 : y)
function f()
    x ? return : y
    return
end
