local a = {}
for i = 1, 16 do a = setmetatable({}, {__call = a}) end
local status, msg = pcall(a)
print("first", status, msg, string.find(msg or "", "too long"))
setmetatable(a, {__call = a})
status, msg = pcall(a)
print("second", status, msg, string.find(msg or "", "too long"))
status, msg = pcall(function () return a() end)
print("third", status, msg, string.find(msg or "", "too long"))
