bundle = Dict{String, Any}(
    "dataset" => ["w1a"],
    "reg" => [0.05],
    "strategy" => [:gradient, :newton, :exact],
)
single = Dict("alpha_setting" => [
    first_component,
    second_component,
    third_component,
    fourth_xx,
])
nested = Dict("first" => 1, "second" => 2, "handlers" => Dict(
    "on_start" => start_fn,
    "on_stop" => stop,
))
tuples = [
    "alpha_key" => (one_component, two_component),
    "beta_key" => (first_beta, second_beta_xxx),
]
direct = map(callback_function, [
    element_one,
    element_two,
    element_three,
    element_four,
    element_five,
])
chain = foo(
    scalar_leading_argument_value,
    "k" => label => [first_value, second_value, third_valuex],
)
