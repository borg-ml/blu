local a = {i = 10}
local self = 20
local f = function() end
function a:x(x) return x + self.i end
function a.y(x) return x + self end
assert(a:x(1) + 10 == a.y(1))
a.t = {i = -100}
a["t"].x = function(self, a, b) return self.i + a + b end
assert(a.t:x(2, 3) == -95)
do
  local a = {x = 0}
  function a:add(x) self.x, a.y = self.x + x, 20; return self end
  assert(a:add(10):add(20):add(30).x == 60 and a.y == 20)
end
local nested = {b = {c = {}}}
function nested.b.c.f1(x) return x + 1 end
function nested.b.c:f2(x, y) self[x] = y end
assert(nested.b.c.f1(4) == 5)
nested.b.c:f2("k", 12)
assert(nested.b.c.k == 12)
function fat(x)
  if x <= 1 then return 1 end
  return x * load("return fat(" .. x - 1 .. ")", "")()
end
assert(load "load 'assert(fat(6)==720)' () ")()
a = load("return fat(5), 3")
local a, b = a()
assert(a == 120 and b == 3)
fat = nil
function deep(n) if n > 0 then return deep(n - 1) else return 101 end end
assert(deep(30000) == 101)
local receiver = {}
function receiver:deep(n) if n > 0 then return self:deep(n - 1) else return 101 end end
assert(receiver:deep(30000) == 101)
local t = nil
local function err_on_n(n)
  if n == 0 then error() end
  return err_on_n(n - 1)
end
do
  local function dummy(n)
    if n > 0 then
      assert(not pcall(err_on_n, n))
      dummy(n - 1)
    end
  end
  dummy(10)
end
local function nested_deep(n) if n > 0 then nested_deep(n - 1) end end
nested_deep(10)
nested_deep(180)
local text = "-- a comment\0\0\0\n  x = 10 + \n23; local a = function () x = 'hi' end; return '\0'"
local function read1(value)
  local i = 0
  return function()
    collectgarbage()
    i = i + 1
    return string.sub(value, i, i)
  end
end
local loaded = assert(load(read1(text), "modname", "t", _G))
assert(loaded() == "\0" and _G.x == 33)
local no_loaded = load(read1(text), "modname", "b", {})
assert(no_loaded == nil)
local no_loaded2 = load(text, "modname", "b")
assert(no_loaded2 == nil)
local function unlpack(t, i)
  i = i or 1
  if i <= #t then return t[i], unlpack(t, i + 1) end
end
local function pack(...) return table.pack(...) end
local function ret2(a, b) return a, b end
local values = {1, 2, 3, 4, false, 10, "alo", false, assert}
local unpacked = pack(unlpack(values))
assert(unpacked.n == #values)
local r1, r2, r3, r4 = ret2(1, 2), ret2(3, 4)
assert(r1 == 1 and r2 == 3 and r3 == 4 and r4 == nil)
do
  local n = 10000
  local function foo()
    if n == 0 then return 1023 end
    n = n - 1
    return foo()
  end
  for i = 1, 100 do foo = setmetatable({}, {__call = foo}) end
  assert(coroutine.wrap(function() return foo() end)() == 1023)
end
do
  local n = 20
  local u = table.pack
  for i = 1, n do u = setmetatable({i}, {__call = u}) end
  local result = u("a", "b", "c")
  assert(result.n == n + 3)
end
do
  local function loop() assert(pcall(loop)) end
  local ok = xpcall(loop, loop)
  assert(not ok)
end
a = assert(load(function() return nil end))
print("EMPTY", a)
print("FIRST", a())
