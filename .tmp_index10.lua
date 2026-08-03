local function doit(s)
  local f, msg = load(s)
  if not f then return msg end
  local ok, value = pcall(f)
  return ok and nil or value
end
print(doit("local a = setmetatable({}, {__index = 10}).x"))
