a +% b
a -% b
a *% b
a +% b +% c
a -% b -% c
a *% b *% c
a +% b *% c -% d
a +%= b
a -%= b
a *%= b
-%(x::Bool) = -%(Int(x))
+%(x::Number, y::Number) = +%(promote(x, y)...)
(*%)(x, y, z...)
for op in (:+, :(+%), :*, :(*%))
end
import .Base: *, *%, +%
mul = *%
midpoint(lo::T, hi::T) where {T<:Integer} = lo +% ((hi -% lo) >>> 0x01)
