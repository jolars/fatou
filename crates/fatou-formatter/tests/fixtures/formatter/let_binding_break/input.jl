let a = 1, b = 2
    a + b
end

let a = 1,
    b = 2
    a + b
end

let alpha = compute(1), beta = compute(2), gamma = compute(3), delta = compute(4), epsilon = 5
    body
end

let aa = 1, bb = 2, cc = 3, dd = 4, ee = 5, ff = 6, gg = 7, hh = 8, ii = 9999999999999999999
    body
end

let aa = 1, bb = 2, cc = 3, dd = 4, ee = 5, ff = 6, gg = 7, hh = 8, ii = 99999999999999999999
    body
end

let counter, accumulator = init(), running_total = zero(T), scratch = allocate(buffer_size_bytes)
    body
end
