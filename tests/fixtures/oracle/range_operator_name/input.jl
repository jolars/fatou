..(x, y) = x == y
..(x)
a .. b
let ..(x, y) = x + y
    @test 3 .. 4 === 7
end
using A: (..)
using A: (..) as twodots
using A: (..), b
import A: (+)
import A: (⋆)
:(using A: (..))
:(using A: (..) as twodots)
f(..)
