# A magic trailing comma after these tails would be absorbed by the tail itself,
# silently rewriting the value into a tuple or growing an iteration clause.
def, name, defval = @something(def_name_defval_from_kwdef_fielddef(kwdef.args[1]), return nothing)

let libccalllazyfoo = LazyLibrary(lclf_path; on_load_callback = () -> global lclf_loaded = true),
    libccalllazybar = LazyLibrary(lclb_path; dependencies = [libccalllazyfoo], on_load_callback = () -> global lclb_loaded = true)
    eval(:(const libccalllazyfoo = $libccalllazyfoo))
end

something_with_a_long_name(first_argument_here, second_argument_here, const bound_value = 1)
something_with_a_long_name(first_argument_here, second_argument_here, local bound_value = 1)
something_with_a_long_name(first_argument_here, second_argument_here, x for x in a_long_iterable)

# A space-form macrocall parses its arguments comma-greedily, so a magic trailing
# comma is drawn into the macro as a tuple element (`@view a[i, :],` reparses as
# `@view((a[i, :],))`) and reflowing must omit it.
some_wrapper(itr, idx) = eachrow(@view parent_container(itr)[idx isa AbstractVector && !(eltype(idx) <: Bool) ? copy(idx) : idx, :])
something_with_a_long_name(first_argument_here, second_argument_here, @view arr[some_long_index, :])

# These tails are self-delimiting, so the trailing comma stays.
something_with_a_long_name(first_argument_here, second_argument_here, inner_call(return x), y)
something_with_a_long_name(first_argument_here, second_argument_here, [return x], trailing_one)
something_with_a_long_name(first_argument_here, second_argument_here, @view(arr[some_index]), tail)
