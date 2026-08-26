rawget({}, "x", 1)
rawset({}, "x", 1, 2)
assert(math.sin(1, 2) == math.sin(1))
table.sort({10, 9, 8, 4, 19, 23, 0, 0}, function(a, b) return a < b end, "extra arg")

local x = "-- a comment\0\0\0\n  x = 10 + \n23; local a = function () x = 'hi' end; return '\0'"
local function read1(value)
    local i = 0
    return function()
        collectgarbage()
        i = i + 1
        return string.sub(value, i, i)
    end
end
local function cannotload(message, a, b)
    assert(not a and string.find(b, message))
end
a = assert(load(read1(x), "modname", "t", _G))
assert(a() == "\0" and _G.x == 33)
assert(debug.getinfo(a).source == "modname")
cannotload("attempt to load a text chunk", load(read1(x), "modname", "b", {}))
cannotload("attempt to load a text chunk", load(x, "modname", "b"))
a = assert(load(function() return nil end))
a()
assert(not load(function() return true end))
local t = {nil, "return ", "3"}
f, msg = load(function() return table.remove(t, 1) end)
assert(f() == nil)
f = load(string.dump(function() return 1 end), nil, "b", {})
assert(type(f) == "function" and f() == 1)
do
    local f = string.dump(function()
        return "01234567890123456789012345678901234567890123456789"
    end)
    f = load(read1(f))
    assert(f() == "01234567890123456789012345678901234567890123456789")
end
x = string.dump(load("x = 1; return x"))
a = assert(load(read1(x), nil, "b"))
assert(a() == 1 and _G.x == 1)
cannotload("attempt to load a binary chunk", load(read1(x), nil, "t"))
cannotload("attempt to load a binary chunk", load(x, nil, "t"))
_G.x = nil
assert(not pcall(string.dump, print))
cannotload("unexpected symbol", load(read1("*a = 123")))
cannotload("unexpected symbol", load("*a = 123"))
cannotload("hhi", load(function() error("hhi") end))
assert(load("return _ENV", nil, nil, 123)() == 123)
local x
XX = 123
local function h()
    local y = x
    return XX
end
local d = string.dump(h)
x = load(d, "", "b")
print("DEBUG", debug.getupvalue(x, 1), debug.getupvalue(x, 2), debug.setupvalue(x, 2, _G), x())
assert(debug.getupvalue(x, 2) == "_ENV")
assert(x() == 123)
