f(x ...)
g(a, b ...)
xs = [a ... , b]
t = (a ... , b)
function collect(head, tail ...)
    tail
end
apply(f, args ...; opts ...)
call(a.b ...)
f(g(x , y) ...)
f(a[i + 1] ...)
f((a+b) ...)
f(A{T , S} ...)
f([1 , 2] ...)
f(very_long_function_name(argument_one, argument_two, argument_three, argument_four, argument_five) ...)
f(; values = very_long_function_name(argument_one, argument_two, argument_three, argument_four, argument_five) ...)
[very_long_function_name(argument_one, argument_two, argument_three, argument_four, argument_five) ...]
[very_long_function_name(argument_one, argument_two, argument_three, argument_four, argument_five) ...][i]
