- A mapping from combination strings to areas:

  ```julia
  euler(Dict("A" => 5, "B" => 3, "A&B" => 1.5))
    α = 1
  ```

- Membership collections:

  ~~~~julia
  euler(Dict("A" => ["x", "y"], "B" => ["y", "z"]))
  ~~~~
