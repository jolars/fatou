for kwdef in fields
    def, name, defval = @something(def_name_defval_from_kwdef_fielddef(kwdef.args[1]), continue)
    def, name, defval = @something(def_name_defval_from_kwdef_fielddef(kwdef.args[1]), break)
    def, name, defval = @something(def_name_defval_from_kwdef_fielddef(kwdef.args[1]), skip && continue)
    def, name, defval = @something(def_name_defval_from_kwdef_fielddef(kwdef.args[1]), stop && break)

    @something(value, # Skip fields without a value.
        continue)
    @something(value, break # Stop at the first missing value.
    )

    @something(value, (continue) # Parentheses protect the keyword from a comma.
    )
    @something(value, begin
        break
    end # The block also protects the keyword from a comma.
    )
end
