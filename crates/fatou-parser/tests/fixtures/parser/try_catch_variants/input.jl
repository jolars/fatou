try
    risky()
catch $e
    handle(e)
end

try
    risky()
catch var"#err"
    handle()
end

try
    x
finally
    cleanup()
catch e
    handle(e)
end
