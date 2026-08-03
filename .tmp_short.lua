local function doit(s)
  local f, msg = load(s)
  if not f then return msg end
  local cond, msg = pcall(f)
  return (not cond) and msg
end
_G.aaa = nil
local message = doit("aaa={}; (aaa or aaa)()")
print(message)
