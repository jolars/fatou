import Base.$(op)
import a.$b
import A.$(b).c
using A.$(b): c
import ..($(x))
import ($(x))
import a.($(b))
import ..(x)
import (x)
import a.(b)
@eval import Base.$(op)
