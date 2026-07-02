result = [
    transform(x, y)
    for x in xslongenoughvalue
    for y in yslongenoughvalue
    if pred(x, y)
]
