local a = {}
setmetatable(a, {})
local ok, message = pcall(function()
  a:bbb(3)
end)
print(ok, message)
