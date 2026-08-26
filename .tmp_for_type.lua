local function first_float()
  for i = 1.0, 10 do return math.type(i) end
end
local function first_int()
  for i = -1, -10, -1.0 do return math.type(i) end
end
print("types", first_float(), first_int())
