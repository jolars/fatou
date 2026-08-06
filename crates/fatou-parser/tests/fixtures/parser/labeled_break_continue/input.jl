break
continue
break outer
continue outer
break outer x
break outer i * 2
break $x
break var"a b"
break 1
for i in 1:10
    if i > 5
        break myblock i * 2
    end
end
@label writeback begin
    n === nothing && break writeback
    if n <= 0
        break writeback
    end
end
val === nothing && break error
ex isa Expr && ex.head === :call || break fail
while c
    break lbl
    continue lbl
end
c in whitespace && (allow_whitespace ? continue : (result = false; break))
x ? break lbl : y
break lbl :sym
break lbl 1:2
