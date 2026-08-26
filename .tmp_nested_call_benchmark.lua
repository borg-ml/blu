local left = {}
local right = {}
local operations = {
    { function(a, b) if not a then return a else return b end end },
    { function(a, b) if a then return a else return b end end },
}
for i = 1, 4000 do left[i] = { i % 2 == 0 } end
for i = 1, 100 do right[i] = { i % 2 == 0 } end
local count = 0
for _, left_value in ipairs(left) do
    for _, right_value in ipairs(right) do
        for _, operation in ipairs(operations) do
            count = count + (operation[1](left_value[1], right_value[1]) and 1 or 0)
            count = count + (operation[1](left_value[1], right_value[1]) and 1 or 0)
        end
    end
end
return count
