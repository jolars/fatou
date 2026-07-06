# short conditions stay flat on the header line
if a && b
    x
end

while ready
    step
end

# a boolean chain that overflows breaks one operand per line at +8, so the
# continuation never shares the +4 indent of the body it guards
while aaaaaaaaaa &&
        bbbbbbbbbb &&
        cccccccccc &&
        dddddddddd &&
        eeeeeeeeee &&
        ffffffffff &&
        ggg
    body_statement
end

if aaaaaaaaaa ||
        bbbbbbbbbb ||
        cccccccccc ||
        dddddddddd ||
        eeeeeeeeee ||
        ffffffffff ||
        gggggggg
    x
end

# an elseif condition breaks the same way; the clause body stays at +4
if q
    a
elseif aaaaaaaaaa &&
        bbbbbbbbbb &&
        cccccccccc &&
        dddddddddd &&
        eeeeeeeeee &&
        ffffffffff &&
        gg
    b
end

# a comparison condition breaks after the operator, continuation at +8
while aaaaaaaaaaaaaaaaaaaaaaaaa <
        bbbbbbbbbbbbbbbbbbbbbbbbb + ccccccccccccccccccccccccccccccccccc
    body
end

# a bracketed predicate call breaks inside its parens: args at +8, close at +4
if some_predicate_function(
        argument_one,
        argument_two,
        argument_three,
        argument_four,
        arg_five,
    )
    body_statement
end

# base indent propagates: nested in a function body the continuation is one level
# deeper than the branch body
function f()
    if aaaaaaaaaa &&
            bbbbbbbbbb &&
            cccccccccc &&
            dddddddddd &&
            eeeeeeeeee &&
            ffffffffff &&
            ggg
        x
    end
end

# a source-broken condition that fits reflows flat (Tenet 1)
if aaaaaaaaaa && bbbbbbbbbb
    x
end

# a catch variable and a for binding are not conditions: no extra indent
try
    x
catch some_caught_error_value
    y
end
