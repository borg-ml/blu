local previous = load("return 1")
x = string.dump(previous)
local x; XX = 123
local function h()
  local y = x
  return XX
end
print("ORIGINAL", debug.getupvalue(h, 1), debug.getupvalue(h, 2), debug.getupvalue(h, 3))
local d = string.dump(h)
x = load(d, "", "b")
print("LOADED", debug.getupvalue(x, 1), debug.getupvalue(x, 2), debug.getupvalue(x, 3))
