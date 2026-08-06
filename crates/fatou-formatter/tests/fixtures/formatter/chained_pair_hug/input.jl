arg = foo(alpha => beta => Dict(:k1 => v1, :k2 => v2, :k3 => v3, :k4 => v4, :k5 => value555555555))
flat = foo(alpha => beta => Dict(:k1 => v1, :k2 => v2))
deep = foo(alpha => beta => gamma => Dict(:k1 => v1, :k2 => v2, :k3 => v3, :k4 => value5555555555555))
elem = [label => alpha => beta => Dict(:k1 => v1, :k2 => v2, :k3 => v3, :k4 => value555555555555555)]
kwarg = foo(x, opts = alpha => beta => Dict(:k1 => v1, :k2 => v2, :k3 => v3, :k44 => value55555555555))
prebroken = foo(alpha =>
  beta =>
  Dict(:k1 => v1, :k2 => v2, :k3 => v3, :k4 => v4, :k5 => value555555555))
plain = foo(alpha => beta => gamma_plain_leaf_value_without_any_bracket_construct_to_hug_at_all_x)
arrowtier = foo(alpha --> beta => Dict(:k1 => v1, :k2 => v2, :k3 => v3, :k4 => v4, :k5 => value5555))
