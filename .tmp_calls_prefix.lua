print("prefix")
function deep(n)
    if n > 0 then return deep(n - 1) else return 101 end
end
assert(deep(30000) == 101)
local receiver = {}
function receiver:deep(n)
    if n > 0 then return self:deep(n - 1) else return 101 end
end
assert(receiver:deep(30000) == 101)
do
    local function loop()
        assert(pcall(loop))
    end
    local err, message = xpcall(loop, loop)
    assert(not err and string.find(message, "error"))
end
do
    local n = 10000
    local function foo()
        if n == 0 then return 1023 end
        n = n - 1
        return foo()
    end
    for _ = 1, 100 do
        foo = setmetatable({}, { __call = foo })
    end
    assert(coroutine.wrap(function() return foo() end)() == 1023)
end
dofile(".tmp_calls_segment.lua")
