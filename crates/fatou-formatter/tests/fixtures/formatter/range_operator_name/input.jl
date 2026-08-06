..(x, y) = x == y
a .. b
let ..(x, y) = x + y
    @test 3 .. 4 === 7
end
using A: (..)
using A: (..) as twodots
import A: (+)
@test repr(:(using A: (..))) == ":(using A: (..))"
