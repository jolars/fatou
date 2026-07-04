result = assemble_pipeline(
    first_stage_transform,
    second_stage_transform,
    third_stage_extra,
    [alpha, beta],
)
config_bounded = configure_run(alpha_setting_name, beta_setting_name, gamma_setting_names, [
    delta,
    epsilon,
])
tight_boundary = configure_runs(
    alpha_setting_name,
    beta_setting_name,
    gamma_setting_names,
    [delta, epsilon],
)
map(
    process_element_callback,
    configuration_alpha,
    configuration_beta,
    configuration_extra,
    [item_one, item_two],
)
wrapped = outer_wrapper(
    inner_builder(first_dimension_specification, second_dimension_specification, [
        row_data,
    ]),
)
deep = wrap_everything(
    construct_matrix(
        first_dimension_specification_value,
        second_dimension_specification_value,
        [row],
    ),
)
totals = accumulate_totals(
    first_partial_result,
    second_partial_result,
    third_partial_extra,
    [
        accumulated_component_value_one,
        accumulated_component_value_two,
        accumulated_component_value_three,
    ],
)
