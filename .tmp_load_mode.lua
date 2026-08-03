local x = "-- comment\0\0\0\n x = 10 + 23; return '\0'"
local function read1(x)
  local i = 0
  return function()
    collectgarbage()
    i = i + 1
    return string.sub(x, i, i)
  end
end
local function show(mode, source)
  local ok, msg = load(source, "modname", mode, {})
  print(mode, ok, msg)
end
show("b", read1(x))
show("b", x)
show("t", string.dump(function() return 1 end))
local y = "-- a comment\0\0\0\n  x = 10 + \n23; \
     local a = function () x = 'hi' end; \
     return '\0'"
local function cannotload(msg, a, b)
  print("cannot", msg, a, b)
  assert(not a and string.find(b, msg))
end
local f = assert(load(read1(y), "modname", "t", _G))
print("first", f(), _G.x)
cannotload("attempt to load a text chunk", load(read1(y), "modname", "b", {}))
cannotload("attempt to load a text chunk", load(y, "modname", "b"))
