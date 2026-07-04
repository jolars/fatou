fence_lowx = tune_model(model_handle, grid_values = (alpha_setting, beta_setting, gammas_x))
fence_high = tune_model(model_handle, grid_values = (
    alpha_setting,
    beta_setting,
    gammas_xy,
))
result = configure(model_object, tolerance = compute_default_tolerance(
    alpha_value,
    beta_value,
    gamma,
))
plot(data_series, options = [
    first_option_value,
    second_option_value,
    third_option_value,
    fourth_opt,
])
fit = optimize(objective_function; settings = build_solver_settings(
    max_iterations,
    convergence_tol,
))
setup(; layers = [
    first_layer_spec,
    second_layer_spec,
    third_layer_spec,
    fourth_layer_spec,
    fifth,
])
run(dataset; verbose = true, callbacks = (
    on_start_handler,
    on_step_handler,
    on_finish_handler_x,
))
prebroken = draw_samples(rng_handle, proposal = build_proposal_kernel(
    step_size_value,
    adaptation_window,
))
configure_solver(
    extremely_long_first_positional_argument_name,
    second_positional;
    tolerances = compute(a, b),
)
small = f(x, kw = [1, 2])
