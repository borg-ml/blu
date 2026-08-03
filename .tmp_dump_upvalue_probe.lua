local a, b = 20, 30
local original = function(x)
  if x == "set" then a = 10 + b; b = b + 1 else return a end
end
print("ORIGINAL", debug.getupvalue(original, 1), debug.getupvalue(original, 2), debug.getupvalue(original, 3))
local x = load(string.dump(original), "", "b", nil)
print("LOADED", x, debug.getupvalue(x, 1), debug.getupvalue(x, 2), debug.getupvalue(x, 3))
print("SET", debug.setupvalue(x, 1, "hi"), debug.setupvalue(x, 2, 13), debug.setupvalue(x, 3, 10))
print("VALUES", x(), x("set"), x())
