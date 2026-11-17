for x in xs
    f(break, x)
    f(continue #= comment =#, x)
    [break]
    {continue}
    (break)
    ready ? break : continue
    break : x
end
