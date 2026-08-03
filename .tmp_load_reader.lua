local x = "-- a comment\0\0\0\n  x = 10 + 23"
local function read1(value)
  local i = 0
  return function()
    collectgarbage()
    i = i + 1
    return string.sub(value, i, i)
  end
end
local a, b = load(read1(x), "modname", "b", {})
print("RESULT", a, b)
