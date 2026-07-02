x = a && b && c
y = aaaaaaaaaaaaaaaaaaa &&
    bbbbbbbbbbbbbbbbbbb &&
    ccccccccccccccccccc &&
    dddddddddddddddddd &&
    eee
z = aaaaaaaaaaaaaaaaaaaa ||
    bbbbbbbbbbbbbbbbbbbb ||
    cccccccccccccccccccc ||
    ddddddddddddddddddd ||
    eeeeeeeeeeeeeeeeeeee
w = aaaaaaaaaaaaaaaaaaaaaaaaa && bbbbbbbbbbbbbbbbbbbbbbbbb ||
    ccccccccccccccccccccccccc && dddddd
r = dataframe |>
    filter(row -> row.value > threshold) |>
    groupby(:category) |>
    combine(:amount => sum)
