short = a.b(1).c(2)

result = dataframe.
    filter(row -> row.age > 18).
    select(:name, :email, :phone_number).
    sort_by(:name)

output = obj.config.
    transform(inputs).
    validate(schema).
    finalize(options, extra_flags, more_flags)

dataframe.
    filter(row -> row.age > 18).
    select(:name, :email, :phone).
    sort_by_columns(:names, :extra)

z = builder.
    configure(settings).result.
    transform(mapping_table).
    build(final_output_target_xyz)

v = repository.
    query(criteria_object).
    transform(pipeline_stages).results.first_matching_items

single = obj.method(
    arg_one,
    arg_two,
    arg_three,
    arg_four,
    arg_five,
    arg_six,
    arg_seven_longer,
)

qualified = Base.Core.Compiler.SomeVeryLongModuleName.AnotherLongName.deeply.nested.field_value

reflowed = obj.aaa(1).bbb(2)
