local x = "-- a comment\0\0\0\n  x = 10 + \n23; local a = function () x = 'hi' end; return '\0'"
local function read1(x)
  local i = 0
  return function()
    collectgarbage()
    i = i + 1
    return string.sub(x, i, i)
  end
end
local function step(name, f)
  local ok, a, b = pcall(f)
  print(name, ok, a, b)
  return ok
end
step("text", function()
  local a,b = load(read1(x), "modname", "t", _G)
  print("text-load", a,b)
  print("text-call", a())
end)
step("binary-reader-text", function()
  local a,b = load(read1(x), "modname", "b", {})
  print("reader", a,b)
end)
step("binary-string-text", function()
  local a,b = load(x, "modname", "b", {})
  print("string", a,b)
end)
step("empty", function()
  local a,b = load(function() return nil end)
  print("empty-load", a,b)
  a()
  print("empty-call")
end)
step("true-reader", function()
  local a,b = load(function() return true end)
  print("true-load", a,b)
end)
