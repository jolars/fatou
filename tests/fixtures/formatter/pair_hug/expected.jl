config = Dict("alpha_setting" => [
    first_component,
    second_component,
    third_component,
    fourth,
])
options = Dict("first" => 1, "second" => 2, "handlers" => Dict(
    "on_start" => start_fn,
    "on_stop" => stop_fn,
))
register!(registry, "transform_pipeline" => Pipeline(
    loader,
    normalizer,
    tokenizer,
    encoder,
))
pairs = broadcast_apply(source_keys .=> [
    first_mapped_value,
    second_mapped_value,
    third_mapped,
])
plot(
    data_series,
    x_axis_column --> [first_value, second_value, third_value, fourth_value_xx],
)
lookup = Dict(
    "configuration_key" => spec_value,
    "other_key_name" => another_specification_val,
)
schedule = assemble(
    "phase_one" => [warmup_steps, ramp_steps],
    final_phase_marker_value_arg_x,
)
mapping = ["alpha_key" => [one_component, two_component], "beta_key" => [
    first_beta,
    second_beta,
]]
fit = optimize(objective; schedule = "warmup_phase" => build_schedule(
    ramp_step_count,
    decay_rate,
))
chained = Dict("outer_key" => "inner_key" => [
    first_element_value,
    second_element_value,
    third,
])
fence_lowx = Dict("configuration_key" => (alpha_setting_value, beta_setting_value, gamma_a))
fence_high = Dict("configuration_key" => (
    alpha_setting_value,
    beta_setting_value,
    gamma_ab,
))
dispatch_event(
    extremely_long_first_positional_argument_name,
    second_positional_name,
    "event_key" => Handler(on_message, on_close),
)
prebroken = Dict("resampled_output" => [
    alpha_component_value,
    beta_component_value_extra_xy,
])
small = Dict("k" => v)
