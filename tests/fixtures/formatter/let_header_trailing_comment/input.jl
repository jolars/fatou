const _UTF8_DFA_TABLE = let # let block rather than function doesn't pollute base
    num_classes = 12
    num_states = 10
    num_classes * num_states
end

let # a bare let whose header is only a comment
    x = 1
    x
end

let x = 1 # a comment after a real binding
    x
end

let x = 1, y = 2 # a comment after the last of several bindings
    x + y
end

for i in 1:3 # a loop header with a trailing comment
    print(i)
end

while cond # a while header with a trailing comment
    step!()
end
