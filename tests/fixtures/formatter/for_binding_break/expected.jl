# short for bindings stay flat on the header line
for x in xs
    body
end

# a call iterable that overflows breaks inside its parens: args at +8, close at
# +4, so the header never shares the +4 indent of the body it guards
for element in some_very_long_iterable_function_call(
        argument_one,
        argument_two,
        argument_three,
    )
    body_statement
end

# a destructuring target with a wide zip iterable breaks the same way
for (index, value) in zip(
        some_long_collection_here,
        another_long_collection_variable_name_x,
    )
    body_statement
end

# an operator-chain iterable breaks after the operator, continuation at +8
for value in first_long_collection_variable_name_here ∪
        second_long_collection_variable_name_x
    body
end

# the iteration operator normalizes to `in` and the wide call still breaks at +8
for item in some_other_long_iterable_function_call(
        argument_one,
        argument_two,
        argument_three_x,
    )
    body
end

# base indent propagates: nested in a function the continuation is one level
# deeper than the loop body
function f()
    for element in some_long_iterable_function_call_name(
            argument_one,
            argument_two,
            arg_three,
        )
        body_statement
    end
end

# a comprehension for-clause is not a loop header: it stays at the comprehension
# indent, never double-indented
result = [
    transform(x)
    for x in some_very_long_iterable_function_call(argument_one, arg_two_x)
]

# a source-broken binding that fits reflows flat (Tenet 1)
for element in short_call(a, b)
    body
end
