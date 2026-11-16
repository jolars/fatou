a .+% b
a .-% b
a .*% b
a .+% b .+% c
a .-% b .-% c
a .*% b .*% c
a .+% b .*% c .-% d
a + b .+% c
a .+% b + c
a * b .*% c
a .*% b * c
a .+% b ^ c
a .+%= b
a .-%= b
a .*%= b
a .+%= b .-%= c .*%= d
a .= b .+%= c
.+%x
.-%x
.+%(x)
.-%(x)
.+%(x, y)
.-%(x, y)
.*%(x, y)
f = .+%
g = .-%
h = .*%
(.+%, .-%, .*%)
:(.+%)
:(.-%)
:(.*%)
:(.+%=)
:(.-%=)
:(.*%=)
[a .+%b]
[a .-%b]
[a .*%b]
[a .+% b]
a.+%b
a.-%b
a.*%b
a.+%=b
a.-%=b
a.*%=b
a .+%₁ b
a .-%₁ b
a .*%₁ b
.+%₁(x)
.-%₁(x)
import Base.+%
import Base.-%
import Base.*%
