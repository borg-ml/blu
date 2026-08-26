local function checkresults(e, ...)
    local t = table.pack(...)
    assert(t.n == #e)
    for i = 1, t.n do
        if type(e[i]) ~= "string" then assert(t[i] == e[i]) end
    end
end
local function checkerror(...)
    local t = table.pack(...)
    assert(t.n == 2 and t[1] == false and type(t[2]) == "string")
end
local function corun(f)
    local co = coroutine.create(f)
    local res = {}
    while coroutine.status(co) == "suspended" do res = { coroutine.resume(co) } end
    assert(coroutine.status(co) == "dead")
    return table.unpack(res)
end
local function colog(f)
    local co = coroutine.create(f)
    local res = {}
    while coroutine.status(co) == "suspended" do
        local run = { coroutine.resume(co) }
        if run[1] then
            table.insert(res, coroutine.status(co) == "suspended" and "yield" or "return")
        else
            table.insert(res, "error")
        end
        table.move(run, 2, #run, 1 + #res, res)
        print(coroutine.status(co), table.unpack(res))
    end
    assert(coroutine.status(co) == "dead")
    return table.unpack(res)
end
checkresults({ true, 42 }, pcall(function() return 42 end))
checkresults({ true, 1, 2, 42 }, pcall(function(a, b) return a, b, 42 end, 1, 2))
checkresults({ true, 2 }, pcall(function(...) return select('#', ...) end, 1, 2))
checkresults({ true, 42 }, pcall(math.abs, -42))
checkresults({ true, 42 }, pcall(setmetatable({}, { __call = function(self, arg) return math.abs(arg) end }), -42))
checkerror(pcall(function() local a = nil / 5 end))
checkerror(pcall(function() select(-100) end))
function stackinfinite() return stackinfinite() end
checkerror(pcall(stackinfinite))
function stackover() return pcall(stackover) end
local res = { pcall(stackover) }
assert(#res == 10000)
checkresults({ "yield", "return", true, 42 }, colog(function() return pcall(function() coroutine.yield() return 42 end) end))
checkresults({ "yield", 1, "return", true, 42 }, colog(function() return pcall(function() coroutine.yield(1) return 42 end) end))
checkresults({ "yield", 1, 2, 3, "return", true, 42 }, colog(function() return pcall(function() coroutine.yield(1, 2, 3) return 42 end) end))
checkresults({ "yield", 1, "yield", 2, "yield", 3, "return", true, 42 }, colog(function() return pcall(function() for i = 1, 3 do coroutine.yield(i) end return 42 end) end))
checkresults({ "yield", "return", true, 1, 2, 3 }, colog(function() return pcall(function() coroutine.yield() return 1, 2, 3 end) end))
checkresults({ "yield", 1, "yield", 2, "return", true, true, 3 }, colog(function() return pcall(function() coroutine.yield(1) return pcall(function() coroutine.yield(2) return 3 end) end) end))
local _error_after_yield = { colog(function() return pcall(function() coroutine.yield() error("foo") end) end) }
local _nested_error_after_yield = { colog(function() return pcall(function() coroutine.yield() return pcall(function() coroutine.yield() error("foo") end) end) end) }
local _outer_error_after_yield = { colog(function() return pcall(function() coroutine.yield() pcall(function() coroutine.yield() error("foo") end) error("bar") end) end) }
local _many_results = { pcall(function() return table.unpack(table.create(100, 'a')) end) }
local _many_coroutine_results = { corun(function() return pcall(function() coroutine.yield() return table.unpack(table.create(100, 'a')) end) end) }
checkresults({ "yield", 1, 2, 3, "return", true }, colog(function() return pcall(coroutine.yield, 1, 2, 3) end))
checkresults({ "yield", 1, 2, 3, "return", true, true, true }, colog(function() return pcall(pcall, pcall, coroutine.yield, 1, 2, 3) end))
checkresults({ "yield", "return", true, true, true, 42 }, colog(function() return pcall(pcall, pcall, function() coroutine.yield() return 42 end) end))
checkresults({ true, 42 }, xpcall(function() return 42 end, error))
checkresults({ true, 1, 2, 42 }, xpcall(function(a, b) return a, b, 42 end, error, 1, 2))
checkresults({ true, 2 }, xpcall(function(...) return select('#', ...) end, error, 1, 2))
checkresults({ "yield", "return", true, 42 }, colog(function() return xpcall(function() coroutine.yield() return 42 end, error) end))
checkresults({ false, "error" }, xpcall(function() error("foo") end, function(err) return err end))
checkresults({ false, "bar" }, xpcall(function() error("foo") end, function(err) return "bar" end))
checkresults({ false, 1 }, xpcall(function() error("foo") end, function(err) return 1, 2 end))
local _xpcall_traceback = { xpcall(function() error("foo") end, debug.traceback) }
checkresults({ false, "traceback" }, xpcall(function() error("foo") end, debug.traceback))
local _xpcall_handler_failure = { xpcall(function() error("foo") end, function(err) error("bar") end) }
checkresults({ false, "error in error handling" }, xpcall(function() error("foo") end, function(err) error("bar") end))
local _xpcall_yield_error = { colog(function() return xpcall(function() coroutine.yield() error("foo") end, function(err) return err end) end) }
local _xpcall_yield_traceback = { colog(function() return xpcall(function() coroutine.yield() error("foo") end, debug.traceback) end) }
local _xpcall_nested_handler_failure = { colog(function() return xpcall(function() return xpcall(function() coroutine.yield() error("foo") end, function(err) error("bar") end) end, error) end) }
local _xpcall_pcall_yield = { colog(function() return xpcall(pcall, function (...) return ... end, function() return pcall(function() coroutine.yield(42) end) end) end) }
checkresults({ false, "error" }, pcall(xpcall, function() return 42 end))
checkresults({ false, "error" }, pcall(xpcall, function() return 42 end, true))
function weird()
    coroutine.yield(weird)
    weird()
end
local _dead_coroutine = { pcall(function() for _ in coroutine.wrap(pcall), weird do end end) }
local _native_throw = { pcall(cxxthrow) }
local co = coroutine.create(function()
    local ok, err = pcall(function() coroutine.yield() end)
    coroutine.yield()
    return ok, err
end)
coroutine.resume(co)
resumeerror(co, "fail")
local _resume_result = { coroutine.resume(co) }
local calllimit = 20000
function recurse(n)
    return n <= 1 and 1 or recurse(n - 1) + 1
end
local direct = recurse(calllimit - 2)
print("direct", direct, calllimit - 2, direct == calllimit - 2)
assert(direct == calllimit - 2)
checkresults({ true, calllimit - 3 }, pcall(recurse, calllimit - 3))
checkerror(pcall(recurse, calllimit - 2))
