valuexyz = compute_branch_scores(alpha_values, beta_values, gamma_values, deltas)[branch_ix]
valuexyza = compute_branch_scores(alpha_values, beta_values, gamma_values, deltas)[branch_ix]
chosen = resolve_candidates(first_candidate_value,
    second_candidate_value, third_candidate)[
    branch_index,
]
entry = combine(alpha_component, beta_component)[first_selected_index_expression, second_selected_index_expression]
picked = combine(alpha_component, beta_component)[extremely_long_first_index_expression_value_padded, extremely_long_second_index_expression_value]
stats = Base.compute_running_moments(sample_values, weight_values, batch_sizes)[moment_index]
param = SomeParametricType{FirstTypeArgument, SecondTypeArgument, ThirdTypeArgument, Fourth}[type_index]
small = lookup(
    alpha_value,
    beta_value,
)[2]
