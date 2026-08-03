local f = function(a) return a + 1 end
print("ORIGINAL", f(3))
local dumped = string.dump(f, true)
print("DUMP", #dumped, dumped:sub(1, 4))
local g, message = load(dumped)
print("LOAD", g, message)
if g then
  local info = debug.getinfo(g)
  print("INFO", info and info.nparams, info and info.nups, info and info.what)
end
if g then print("RUN", g(3)) end
