local values = {}
for i = 1, 400000 do
    values[#values + 1] = { i, false }
end
return #values
