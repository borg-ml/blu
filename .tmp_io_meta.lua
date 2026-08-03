local mt = getmetatable(io.stdin)
print("META", mt, mt and mt.__gc)
local ok, message = pcall(function() return mt.__gc() end)
print("CALL", ok, message)
