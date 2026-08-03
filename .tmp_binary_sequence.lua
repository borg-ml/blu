global <const> *
local x = "-- a comment\0\0\0\n  x = 10 + \n23; local a = function () x = 'hi' end; return '\0'"
local function read1(value)
  local index = 0
  return function()
    collectgarbage()
    index = index + 1
    return string.sub(value, index, index)
  end
end
local a = assert(load(read1(x), "modname", "t", _G))
print("TEXT", a(), _G.x)
local f = assert(load(function() return nil end))
f()
f = load(string.dump(function() return 1 end), nil, "b", {})
print("DUMP1", f())
local long = string.dump(function()
  return "01234567890123456789012345678901234567890123456789"
end)
f = load(read1(long))
print("DUMP2", f())
local dumped = string.dump(load("x = 1; return x"))
local restored, message = load(read1(dumped), nil, "b")
print("RESTORED", restored, message)
if restored then
  local ok, result = pcall(restored)
  print("RESULT", ok, result, _G.x)
end
