for outer i in 1:3 end
for outer i = 1:3 end
for outer i in 1:3, outer j in 1:4 end
for i in 1:3, outer j in 1:4 end
for outer (i, j) in xs end
for outer [i] in xs end
for outer i::Int in xs end
for outer i[1] in xs end
for outer i in 1:3 x += i end
[x for outer i in 1:3]
(x for outer i in 1:3)
for outer in 1:3 end
for outer = 1:3 end
for outer in outer end
outer = 1
outer(x)
x = outer
for outer outer in xs end
for outer x in xs, y in ys end
for outer $ i = 1:3 end
for outer + i = 1:3 end
for outer.x in xs end
for outer[1] in xs end
for outer(1) in xs end
for outer::Int in xs end
for outer 1 in xs end
for outer :x in xs end
for outer var"x" in xs end
