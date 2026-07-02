a = [x for x in v if x > 0]
result = [
    transform(x, y)
    for x in xslongenoughvalue
    for y in yslongenoughvalue
    if predicate(x, y)
]
