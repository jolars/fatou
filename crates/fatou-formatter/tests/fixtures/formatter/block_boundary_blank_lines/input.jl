begin


    # First group.
    x = 1 # Keep this attached.


    # Second group.
    y = 2
    # Final comment.


end

quote

    x

end

let x = 1

    x

end

for i in 1:3

    while ready(i)

        step(i)

    end

end

if first_condition

    first_action()

elseif second_condition

    second_action()

else

    last_action()

end

try

    risky()

catch err

    handle(err)

else

    success()

finally

    cleanup()

end

struct Data

    x::Int

    function Data(x)

        new(x)

    end

end

macro identity_expr(x)

    esc(x)

end

map(xs) do x

    x + 1

end

begin

    #= Only a comment.
Its content stays verbatim.
=#

end

function empty()


end

if empty_branch

else

    # Only a line comment.

end
