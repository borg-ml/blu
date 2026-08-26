local maxi = math.maxinteger
local mini = math.mininteger
local i = maxi - 6
local j = __blu_internal_coerce_numeric_for_index(i, 3)
print("coerce", i, j, math.type(j))
local c = 0
for n = i, maxi, 3 do
  c = c + 1
  print("loop", c, n, math.type(n))
  if c == 4 then break end
end
print("other", mini + 4, __blu_internal_coerce_numeric_for_index(mini + 4, -2))
