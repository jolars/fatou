module A

domaths(x::Number) = x + 5

domaths(x::Int) = x + 10

function domaths(x::String)

    return "A string is a one-dimensional extended object postulated in string theory."

end

function f(x)

    y = x + 1

    return y

end

dophilosophy() = "Why?"

end
