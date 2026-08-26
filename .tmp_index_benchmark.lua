local values = { { "a", true } }
local result = 0
for i = 1, 400000 do
    local value = values[1]
    if value[2] then result = result + 1 end
end
return result
