for i in 1:10
    if i > 5
        break myblock i * 2
    end
end
@label writeback begin
    n === nothing && break writeback
    break writeback
end
while c
    continue outer
    break
end
val === nothing && break error
ex isa Expr && ex.head === :call || break fail
c in whitespace && (allow_whitespace ? continue : (result = false; break))
x ? break lbl : y
