local basiccases = {
    { "nil", nil },
    { "false", false },
    { "true", true },
    { "10", 10 },
    { "(0 == 0)", true },
}
local binops = {
    { " and ", function(a, b) if not a then return a else return b end end },
    { " or ", function(a, b) if a then return a else return b end end },
}
local cases = {}
local function createcases(n)
    local result = {}
    for i = 1, n - 1 do
        for _, left in ipairs(cases[i]) do
            for _, right in ipairs(cases[n - i]) do
                for _, op in ipairs(binops) do
                    local expression = "(" .. left[1] .. op[1] .. right[1] .. ")"
                    result[#result + 1] = { expression, op[2](left[2], right[2]) }
                    result[#result + 1] = { "not" .. expression, not op[2](left[2], right[2]) }
                end
            end
        end
    end
    return result
end
cases[1] = basiccases
for i = 2, 4 do cases[i] = createcases(i) end
local count = 0
for n = 1, 4 do
    for _, case in pairs(cases[n]) do
        count = count + #case[1]
    end
end
return count
