try
    risky()
catch e
    handle(e)
finally
    cleanup()
end

try
    f()
catch
    g()
else
    h()
end
