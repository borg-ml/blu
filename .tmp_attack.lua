local function countentries(t)
  local e = 0
  for _ in pairs(t) do e = e + 1 end
  return e
end
local a = {}
for i = 1, 2^11 - 1 do a[i .. ""] = true end
for i = 1, 1e5 do
  local key = i .. "."
  a[key] = true
  a[key] = nil
end
assert(countentries(a) == 2^11 - 1)
print("OK")
