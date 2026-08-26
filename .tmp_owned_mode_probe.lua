local file = os.tmpname()
local f = assert(io.open(file, "w")); f:close()
local modes = {"rw", "rb+", "r+bk", "", "+", "b", "r+b", "r+", "rb"}
for _, mode in ipairs(modes) do
  local ok, a, b = pcall(io.open, file, mode)
  print(mode, tostring(ok), type(a), tostring(a), type(b), tostring(b))
  if ok and a then a:close() end
end
os.remove(file)
local ok, a, b = pcall(load, "", "", "B")
print("load-B", tostring(ok), type(a), tostring(a), type(b), tostring(b))
