local file = os.tmpname()
local f = assert(io.open(file, "w")); f:write("data\n"); f:close()
local otherfile = os.tmpname()
print("before-gc")
collectgarbage()
for i=1,120 do
  for j=1,5 do
    io.input(file)
    assert(io.open(file, 'r'))
    io.lines(file)
  end
  collectgarbage()
end
print("after-gc")
io.input():close()
io.close()
print("after-close")
local a,b,c = os.rename(file, otherfile)
print("rename1", tostring(a), type(b), tostring(b), type(c), tostring(c))
local d,e,g = os.rename(file, otherfile)
print("rename2", tostring(d), type(e), tostring(e), type(g), tostring(g))
assert(a)
assert(not d)
print("gc passed")
