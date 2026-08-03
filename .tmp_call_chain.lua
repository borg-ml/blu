for n = 1, 20 do
  local a = {}
  for i = 1, n do a = setmetatable({}, {__call = a}) end
  local ok, msg = pcall(a)
  print(n, ok, msg)
end
local a = {}
for i = 1, 16 do a = setmetatable({}, {__call = a}) end
setmetatable(a, {__call = a})
local ok, msg = pcall(function() return a() end)
print("tail", ok, msg)
