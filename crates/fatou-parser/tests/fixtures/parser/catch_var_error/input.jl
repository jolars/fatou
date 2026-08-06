try
    risky()
catch e
    handle(e)
end

try
    risky()
catch e + 3
    handle()
end
