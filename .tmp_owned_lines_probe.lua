local file = os.tmpname()
local otherfile = os.tmpname()
local f = assert(io.open(otherfile, "w"))
f:write("one\ntwo\nthree\nfour\nfive\nsix\n")
f:close()
assert(not pcall(io.lines, "non-existent-file"))
assert(os.rename(otherfile, file))
io.output(otherfile)
local n = 0
local it = io.lines(file)
while it() do n = n + 1 end
print("count", n)
local function testerr (msg, fn, ...)
 local ok, err = pcall(fn, ...)
 print("err", tostring(ok), type(err), tostring(err))
 return not ok and string.find(err, msg, 1, true)
end
assert(n == 6)
assert(testerr("file is already closed", it))
assert(testerr("file is already closed", it))
print("closed iterator passed")
n = 0
for l in io.lines(file) do io.write(l, "\n"); n=n+1 end
io.close()
print("copy1", n)
n = 0
local g = assert(io.open(file))
io.output(otherfile)
for l in g:lines() do io.write(l, "\n"); n=n+1 end
g:close(); io.close()
print("copy2", n)
assert(n == 6)
os.remove(file)
os.remove(otherfile)
local ok, err = pcall(io.close, g)
print("closed-io", tostring(ok), type(err), tostring(err))
