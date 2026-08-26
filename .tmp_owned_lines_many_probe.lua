local otherfile = os.tmpname()
io.output(otherfile)
io.write(string.rep("a", 300), "\n")
io.close()
local t = {}
for i = 1, 250 do t[i] = 1 end
local ok, a, b = pcall(function() return {io.lines(otherfile, table.unpack(t))()} end)
print("many", tostring(ok), type(a), type(b), tostring(b))
if ok then print("count", #a, tostring(a[1]), tostring(a[#a])) end
t[#t + 1] = 1
local ok2, err2 = pcall(io.lines, otherfile, table.unpack(t))
print("too-many", tostring(ok2), type(err2), tostring(err2))
os.remove(otherfile)
