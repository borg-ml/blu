local file = os.tmpname()
local F = nil
do
  local f <close> = assert(io.open(file, "w"))
  F = f
end
print("closed", tostring(F), io.type(F), type(getmetatable(F).__close))
assert(tostring(F) == "file (closed)")
assert(os.remove(file))
print("scope passed")
