fence_low = interpolate_grid(sample_points, weights; method = :cubic, tol = tolex)[probe_ix]
fence_high = interpolate_grid(
    sample_points,
    weights;
    method = :cubic,
    tol = toler,
)[probe_ix]
kwonly = build_solver_registry(;
    default_backend = :dense_lut,
    fallback = :qr_pivot,
)[backend]
phug = configure_run(experiment; stages = [
    warmup_phase,
    measure_phase,
    report_ph,
])[stage_id]
tail_break = summarize(
    records;
    by = grouping_key,
)[very_long_selector_expression_for_this_row]
