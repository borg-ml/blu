_ENV.GLOB1 = 0
local basiccases = {
  {"nil", nil}, {"false", false}, {"true", true},
  {"10", 10}, {"(0==_ENV.GLOB1)", true},
}
basiccases[2][1] = "F"
local prog = [[
    local F <const> = false
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
          local t = { "(" .. v1[1] .. op[1] .. v2[1] .. ")", op[2](v1[2], v2[2]) }
          res[#res + 1] = t
          res[#res + 1] = {"not" .. t[1], not t[2]}
        end
      end
    end
  end
  return res
end
cases[1] = basiccases
for i = 2, 4 do cases[i] = createcases(i) end
for n = 1, 4 do
  for _, v in pairs(cases[n]) do
    local s = v[1]
    local p = load(string.format(prog, s, s), "")
    IX = false
    assert(p() == v[2] and IX == not not v[2])
  end
end
return true
