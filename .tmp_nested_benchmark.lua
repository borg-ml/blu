local left = {}
local right = {}
local operations = { 1, 2 }
for i = 1, 4000 do left[i] = i end
for i = 1, 100 do right[i] = i end
local count = 0
for _, left_value in ipairs(left) do
    for _, right_value in ipairs(right) do
        for _, operation in ipairs(operations) do
            count = count + left_value + right_value + operation
        end
    end
end
return count
