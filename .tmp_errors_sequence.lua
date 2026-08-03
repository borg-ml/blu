local mt = getmetatable(_G) or {}
local oldmm = mt.__index
mt.__index = nil

local function doit(s)
  local f, msg = load(s)
  if not f then return msg end
  local cond, msg = pcall(f)
  return (not cond) and msg
end
local function check(s, expected)
  local m = doit(s)
  print(expected, m, m and string.find(m, expected, 1, true) or false)
end
assert(doit("error('hi', 0)") == "hi")
assert(doit("error()") == nil)
assert(doit("table.unpack({}, 1, n=2^30)"))
assert(doit("a=math.sin()"))
assert(not doit("tostring(1)") and doit("tostring()"))
assert(doit("tonumber()"))
assert(doit("repeat until 1; a"))
assert(doit("return;;"))
assert(doit("assert(false)"))
assert(doit("assert(nil)"))
assert(doit("function a (... , ...) end"))
assert(doit("function a (, ...) end"))
assert(doit("local t={}; t = t[#t] + 1"))
assert(doit([[local a = {4

]]))
assert(doit([[::A:: a = 1
::A::]]))
assert(doit([[a = 1
goto A
do ::A:: end]]))
check("a = {} + 1", "arithmetic")
check("a = {} | 1", "bitwise operation")
check("a = {} < 1", "attempt to compare")
check("a = {} <= 1", "attempt to compare")
check("aaa=1; bbbb=2; aaa=math.sin(3)+bbbb(3)", "global 'bbbb'")
check("aaa=1; local aaa,bbbb=2,3; aaa = bbbb(1) or aaa(3)", "local 'bbbb'")
assert(not string.find(doit("aaa={13}; local bbbb=1; aaa[bbbb](3)"), "'bbbb'"))
_G.aaa, _G.bbbb = nil
check("local a; a(13)", "local 'a'")
check("local a = setmetatable({}, {__add = 34}); a = a + 1", "metamethod 'add'")
check("local a = setmetatable({}, {__lt = {}}); a = a > a", "metamethod 'lt'")
check("local a={}; return a.bbbb(3)", "field 'bbbb'")
check("aaa={}; do local aaa=1 end; return aaa:bbbb(3)", "method 'bbbb'")
check("aaa = #print", "length of a function value")
check("aaa = #3", "length of a number value")
_G.aaa = nil
check("aaa.bbb:ddd(9)", "global 'aaa'")
check("local aaa={bbb=1}; aaa.bbb:ddd(9)", "field 'bbb'")
check("local aaa={bbb={}}; aaa.bbb:ddd(9)", "method 'ddd'")
check("local a,b,c; (function () a = b+1.1 end)()", "upvalue 'b'")
assert(not doit("local aaa={bbb={ddd=next}}; aaa.bbb:ddd(nil)"))
check("local a,b,cc; (function () a = cc[1] end)()", "upvalue 'cc'")
check("local a,b,cc; (function () a.x = 1 end)()", "upvalue 'a'")
check("local _ENV = {x={}}; a = a + 1", "global 'a'")
check("BB=1; local aaa={}; x=aaa+BB", "local 'aaa'")
check("aaa={}; x=3.3/aaa", "global 'aaa'")
check("aaa=2; BB=nil;x=aaa*BB", "global 'BB'")
check("aaa={}; x=-aaa", "global 'aaa'")
check("aaa=1; local aaa,bbbb=2,3; aaa = math.sin(1) and bbbb(3)", "local 'bbbb'")
check("local a,b,c,f = 1,1,1; f((a and b) or c)", "local 'f'")
check("local a,b,c = 1,1,1; ((a and b) or c)()", "call a number value")
assert(not string.find(doit("aaa={}; x=(aaa or aaa)+(aaa and aaa)"), "'aaa'"))
assert(not string.find(doit("aaa={}; (aaa or aaa)()"), "'aaa'"))
check("print(print < 10)", "function with number")
check("print(print < print)", "two function values")
check("print('10' < 10)", "string with number")
check("print(10 < '23')", "number with string")
for _, source in ipairs({
  "local a = 2e100 ~ 1",
  "string.sub('a', 2.0^100)",
  "return 34 >> {}",
  "aaa = 24 // 0",
  "aaa = 1 % 0",
}) do
  check(source, "error")
end
check("local a = setmetatable({}, {__index = 10}).x", "attempt to index a number value")
