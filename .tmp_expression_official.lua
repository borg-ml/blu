local basiccases = {
    { "nil", nil }, { "false", false }, { "true", true },
    { "10", 10 }, { "(0 == 0)", true },
}
local binops = { " and ", " or " }
local cases = {}
local function createcases(n)
    local result = {}
    for i = 1, n - 1 do
        for _, left in ipairs(cases[i]) do
            for _, right in ipairs(cases[n - i]) do
                for _, op in ipairs(binops) do
                    local expression = "(" .. left[1] .. op .. right[1] .. ")"
                    result[#result + 1] = { expression, true }
                    result[#result + 1] = { "not" .. expression, false }
                end
            end
        end
    end
    return result
end
cases[1] = basiccases
for i = 2, 4 do cases[i] = createcases(i) end
local prog = [[
    local k10 <const> = 10
    if %s then IX = true end
    return %s
  ]]
local count = 0
for n = 1, 4 do
    for _, v in pairs(cases[n]) do
        local s = v[1]
        local p = assert(load(string.format(prog, s, s), ""))
        IX = false
        local result = p()
        assert(result == result)
        count = count + 1
    end
end
return count
