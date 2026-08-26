print("testing pcall")
function checkresults(e, ...)
    local t = table.pack(...)
    print("count", t.n, #e, t[1], t[2])
    assert(t.n == #e)
    for i = 1, t.n do
        if type(e[i]) ~= "string" then assert(t[i] == e[i]) end
    end
end
checkresults({ true, 42 }, xpcall(function() return 42 end, error))
checkresults({ true, 1, 2, 42 }, xpcall(function(a, b) return a, b, 42 end, error, 1, 2))
checkresults({ true, 2 }, xpcall(function(...) return select('#', ...) end, error, 1, 2))
checkresults({ "yield", "return", true, 42 }, (function()
    local co = coroutine.create(function() return xpcall(function() coroutine.yield() return 42 end, error) end)
    local res = {}
    while coroutine.status(co) == "suspended" do
        local run = { coroutine.resume(co) }
        table.insert(res, coroutine.status(co) == "suspended" and "yield" or "return")
        table.move(run, 2, #run, 1 + #res, res)
    end
    return table.unpack(res)
end)())
checkresults({ false, ".tmp_xpcall_sequence.luau:18: foo" }, xpcall(function() error("foo") end, function(err) return err end))
checkresults({ false, "bar" }, xpcall(function() error("foo") end, function(err) return "bar" end))
checkresults({ false, 1 }, xpcall(function() error("foo") end, function(err) return 1, 2 end))
checkresults({ false, ".tmp_xpcall_sequence.luau:21: foo\n.tmp_xpcall_sequence.luau:21\n.tmp_xpcall_sequence.luau:21\n" }, xpcall(function() error("foo") end, debug.traceback))
checkresults({ false, "error in error handling" }, xpcall(function() error("foo") end, function(err) error("bar") end))
print("done")
