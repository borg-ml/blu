local source = "x = 1; return x"
local dumped = string.dump(load(source))
local index = 0
local function reader()
  collectgarbage()
  index = index + 1
  return dumped:sub(index, index)
end
local a = assert(load(reader, nil, "b"))
print("BEFORE", debug.getupvalue(a, 1), _G.x)
assert(a() == 1 and _G.x == 1)
print("AFTER", _G.x)
