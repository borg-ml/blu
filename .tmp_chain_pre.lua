local n = 10000
local function foo()
  if n == 0 then return 1023 else n = n - 1; return foo() end
end
for i = 1, 15 do foo = setmetatable({}, {__call = foo}) end
assert(coroutine.wrap(function() return foo() end)() == 1023)
local N = 15
local u = table.pack
for i = 1, N do u = setmetatable({i}, {__call = u}) end
local Res = u("a", "b", "c")
assert(Res.n == N + 3)
for i = 1, N do assert(Res[i][1] == i) end
assert(Res[N + 1] == "a" and Res[N + 2] == "b" and Res[N + 3] == "c")
local function u(...)
  local n = debug.getinfo(1, "t").extraargs
  assert(select("#", ...) == n)
  return n
end
for i = 0, N do
  assert(u() == i)
  u = setmetatable({}, {__call = u})
end
local a = {}
for i = 1, 16 do a = setmetatable({}, {__call = a}) end
local status, msg = pcall(a)
print("first", status, msg)
setmetatable(a, {__call = a})
status, msg = pcall(a)
print("second", status, msg)
status, msg = pcall(function () return a() end)
print("third", status, msg)
local source = "return '\0'"
local function reader()
  local i = (i or 0) + 1
  _G.i = i
  collectgarbage()
  return string.sub(source, i, i)
end
local f, err = load(reader, "modname", "t", _G)
print("loadafter", f, err)
