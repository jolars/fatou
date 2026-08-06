import .A
import ..A
import ...A
import A as B
import A, B
import A: x as y
import A.B: x as y
using A.B: c
using A: x as y
import A.B.C: x, y as z
