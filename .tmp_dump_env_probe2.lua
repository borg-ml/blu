local x
XX = 123
local function h()
  local y = x
  return XX
end
local d = string.dump(h)
local loaded, load_message = load(d, "", "b")
print("LOADED", loaded ~= nil, load_message)
if loaded then
  print("UPVALUES", debug.getupvalue(loaded, 1), debug.getupvalue(loaded, 2))
  print("SET", debug.setupvalue(loaded, 2, _G))
  print("RESULT", loaded())
end
