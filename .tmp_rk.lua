local t = {}
_G.x = 3.3
for i = 1, 1000 do t[i] = "aaa = x" .. i end
local s = table.concat(t, "; ")
local function doit(program)
  local f, message = load(program)
  if not f then return message end
  local ok, result = pcall(f)
  return (not ok) and result
end
local function checkmessage(program, expected)
  local result = doit(program)
  print("MESSAGE", result)
  print("MATCH", string.find(result, expected, 1, true))
end
checkmessage(s .. "; aaa = bbb + 1", "global 'bbb'")
checkmessage("local _ENV=_ENV;" .. s .. "; aaa = bbb + 1", "global 'bbb'")
checkmessage(s .. "; local t = {}; aaa = t.bbb + 1", "field 'bbb'")
checkmessage(s .. "; local t = {}; t:bbb()", "method 'bbb'")
checkmessage(s .. "; local x,y = {},1; x.a()", "field 'a'")
checkmessage([[aaa=9
repeat until 3==3
local x=math.sin(math.cos(3))
if math.sin(1) == x then return math.sin(1) end
local a,b = 1, {
  {x='a'..'b'..'c', y='b', z=x},
  {1,2,3,4,5} or 3+3<=3+3,
  3+1>3+1,
  {d = x and aaa[x or y]}}
]], "global 'aaa'")
