local ok, message = xpcall(function() error("foo") end, function() error("bar") end)
print(ok, message)
local ok2, message2 = pcall(function() return xpcall(function() error("foo") end, function() error("bar") end) end)
print(ok2, message2)
