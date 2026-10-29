!!! note "Gallery"
    A gallery example:
    ```@example gallery
    α = 1
    if α > 0
        β = 2
    end
    ```
    More prose.

!!! tip
    ~~~~julia-repl
    julia> α = 1
    1
    ~~~~

!!! warning "Blank lines"
    ```jldoctest gallery; filter = r"α"
    α = 1


    α + 1
    # output
    2
    ```

!!! note "Tabbed prefix"
	```@example gallery
	α = 1
	    β = 2
	```

!!! note "Mixed prefixes"
    ~~~julia
	α = 1
      β = 2
	~~~

!!! note "Math"
    ~~~math
    α^2 + β^2
    γ^2
    ~~~

!!! note "Padded fence"
       ```julia
    α = 1
    ```

!!! note "Indented delimiters are code"
    ```julia
      ```
    α = 1
    ```

!!! note "Empty fence"
    ```@example gallery
    ```

After the admonitions.
