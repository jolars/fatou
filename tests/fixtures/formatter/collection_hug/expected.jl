fencecoll_low = [alpha_component_value, beta_component_value, combine_tail(gamma_input, dx)]
fencecol_high = [alpha_component_value, beta_component_value, combine_tail(
    gamma_input,
    dxy,
)]
coefficients = [first_coefficient, second_coefficient, compute_remaining(
    alpha,
    beta,
    gamma,
    delta,
)]
config = (verbose_flag, output_directory, build_transform_pipeline(
    stage_one,
    stage_two,
    stage_three,
))
settings = (name = experiment_name, values = [
    first_value,
    second_value,
    third_value,
    fourth_value_x,
])
basis = {first_generator_element, second_generator_element, [
    alpha_span,
    beta_span,
    gamma_span_x,
]}
singleton = (compute_extremely_long_intermediate_result(
    alpha_input,
    beta_input,
    gamma_input_xy,
),)
prebroken = [first_component_entry, second_component_entry, assemble_tail_component(
    theta_value,
    kappa_value,
)]
mixture_components = [
    first_component_weight_value,
    second_component_weight_value,
    build_tail(a, b),
]
picked = [first_coefficient, second_coefficient, compute_remaining(
    alpha,
    beta,
    gamma,
    delta_val,
)][chosen_index]
tiny = [a, b, g(1)]
