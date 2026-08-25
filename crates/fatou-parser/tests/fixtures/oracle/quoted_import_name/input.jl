using :A
using A: :b
import :A
import A: :b
using (:C)
using A: :(d)
using A: (:e)
using : F

# Dotted quoted components remain valid.
using A: :+
import A.:+
using A.(:f)
