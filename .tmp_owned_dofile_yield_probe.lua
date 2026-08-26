local file = os.tmpname()
local f = assert(io.open(file, "w"))
f:write[[
local x, z = coroutine.yield(10)
local y = coroutine.yield(20)
return x + y * z
]]
assert(f:close())
f = coroutine.wrap(dofile)
local a = f(file)
local b = f(100, 101)
local c = f(200)
print("yield", tostring(a), tostring(b), tostring(c))
assert(a == 10 and b == 20 and c == 100 + 200 * 101)
assert(os.remove(file))
print("yield passed")
