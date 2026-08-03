local t = {}
local res, msg = pcall(assert, false, "X", t)
print("STRING", res, msg, type(msg), msg == "X")
res, msg = pcall(function() assert(false) end)
local line = string.match(msg, "%w+%.lua:(%d+): assertion failed!$")
print("DEFAULT", res, msg, type(msg), line, tonumber(line), debug.getinfo(1, "l").currentline - 2)
res, msg = pcall(assert, false, t)
print("TABLE", res, msg, type(msg), msg == t)
