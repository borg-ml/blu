local debug = require "debug"
local basiccases = {
  {"nil", nil},
  {"false", false},
  {"true", true},
  {"10", 10},
  {"(0==0)", 0 == 0},
}
local prog = [[
    local k10 <const> = 10
    if %s then IX = true end
    return %s
  ]]
local binops <const> = {
  {" and ", function (a,b) if not a then return a else return b end end},
  {" or ", function (a,b) if a then return a else return b end end},
}
local cases <const> = {}
local function createcases (n)
  local res = {}
  for i = 1, n - 1 do
    for _, v1 in ipairs(cases[i]) do
      for _, v2 in ipairs(cases[n - i]) do
        for _, op in ipairs(binops) do
            local t = {
              "(" .. v1[1] .. op[1] .. v2[1] .. ")",
              op[2](v1[2], v2[2])
            }
            res[#res + 1] = t
            res[#res + 1] = {"not" .. t[1], not t[2]}
        end
      end
    end
  end
  return res
end
local level = 4
cases[1] = basiccases
for i = 2, level do cases[i] = createcases(i) end
local i = 0
for n = 1, level do
  for _, v in pairs(cases[n]) do
    local s = v[1]
    local p = load(string.format(prog, s, s), "")
    IX = false
    assert(p() == v[2] and IX == not not v[2])
    i = i + 1
  end
end
return i
