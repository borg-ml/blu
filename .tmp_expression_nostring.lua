local basiccases = { { nil }, { false }, { true }, { 10 }, { true } }
local binops = { {}, {} }
local cases = {}
local function createcases(n)
    local result = {}
    for i = 1, n - 1 do
        for _, left in ipairs(cases[i]) do
            for _, right in ipairs(cases[n - i]) do
                for _, op in ipairs(binops) do
                    result[#result + 1] = { 0, false }
                    result[#result + 1] = { 0, true }
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
    for _, case in pairs(cases[n]) do count = count + #case end
end
return count
