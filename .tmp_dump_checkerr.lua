local f = function(a) return a + 1 end
f = assert(load(string.dump(f, true)))
print("NUMBER", f(3))
local ok, message = pcall(f, {})
print("TABLE", ok, message)
if message then
  print("TRACE_MATCH", string.find(message, "^%?:%-1:"))
end
