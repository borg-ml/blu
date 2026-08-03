local function read1(value)
  local i = 0
  return function()
    collectgarbage()
    i = i + 1
    return string.sub(value, i, i)
  end
end
local source = load("x = 1; return x")
local dumped = string.dump(source)
for _, mode in ipairs({"b", "t", nil}) do
  local a, b = load(read1(dumped), nil, mode)
  print("MODE", mode, a, b)
end
