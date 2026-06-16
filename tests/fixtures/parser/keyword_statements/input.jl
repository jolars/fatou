function f(x)
    if x < 0
        return
    end
    return x + 1
end

while x > 0
    if x == 3
        break
    end
    continue
end

const c = 1
global a, b
local y = 2

import A
import A: b
using A.B
using A: b, c
export a, b
