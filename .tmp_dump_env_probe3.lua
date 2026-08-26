x = string.dump(load("x = 1; return x"))
_G.x = nil
local x
XX = 123
local function h()
  local y = x
  return XX
end
local d = string.dump(h)
x = load(d, "", "b")
print("UPVALUES", debug.getupvalue(x, 1), debug.getupvalue(x, 2))
print("SET", debug.setupvalue(x, 2, _G))
print("RESULT", x(), x())
