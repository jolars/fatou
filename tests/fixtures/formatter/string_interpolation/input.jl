flat = "hello $(name), welcome"
wide = "hello $(name) world $(another_variable) and $(yet_another_one) plus $(one_more_variable_here)"
spaced = "interp with spaces $( y + z )"
prebroken = "a $(
    inner_value
) b"
bare = "a $b c"
call = "value is $(compute_something(arg_one, arg_two, arg_three, arg_four, arg_five, arg_six, seven))"
nested = "outer $("inner  spaces")  tail"
cmd = `run --flag $(long_variable_one) --other $(long_variable_two) --third $(long_variable_three_x)`
comment = "a $(f(y) #= keep =#) b"
