break lbl, y
continue lbl, y
break $lbl, y
continue var"label name", y
break lbl, y = values
continue lbl, y = values
result = break lbl, y
return continue lbl, y
break lbl,
    y
begin
    break lbl, y
    continue lbl, y
end
(break lbl, y)
(continue lbl, y)
f(break lbl, y)
f(continue lbl, y)
[break lbl, y]
[continue lbl, y]
break lbl x y
continue lbl x y
begin
    break lbl x y
    after_break
    continue lbl x y
    after_continue
end
(break lbl x y)
f(continue lbl x y)
[break lbl x y]
break lbl x #= comment =# y
continue lbl #= comment =# x
break lbl x y; after
continue lbl x; after
break lbl x y