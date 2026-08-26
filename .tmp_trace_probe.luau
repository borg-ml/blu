local function target()
    error("foo")
end
local ok, message = xpcall(target, debug.traceback)
print(ok)
print(message)
