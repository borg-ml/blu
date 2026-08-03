local original = assert(load("x = 1; return x"))
local dumped = string.dump(original)
local index = 0
local function reader()
  collectgarbage()
  index = index + 1
  return dumped:sub(index, index)
end
local restored, message = load(reader, nil, "b")
print("RESTORED", restored, message)
if restored then
  local ok, result = pcall(restored)
  print("RESULT", ok, result, x)
end
