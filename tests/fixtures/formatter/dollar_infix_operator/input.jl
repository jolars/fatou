a $ b
c = a $b
for outer $ i = 1:3
    @test 1 $ 2 in 1:3
end
x = [a $b]
y = [a $ b]
ex = :($a $ $b)
z = $(a, b)
w = $(x...,)
