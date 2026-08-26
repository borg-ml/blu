local function func2close(f)
  return setmetatable({}, {__close = f})
end

local extrares
local function check(body, extra, ...)
  local t = table.pack(...)
  local co = coroutine.wrap(body)
  if extra then
    extrares = co()
    print("EXTRA", extrares)
  end
  local res = table.pack(co())
  print("RES", res.n, res[1], res[2], res[3])
  local res2 = table.pack(co())
  print("RES2", res2.n, res2[1], res2[2], res2[3], "EXTRA", extrares)
end

local function foo()
  local x <close> = func2close(coroutine.yield)
  local extra <close> = func2close(function(self)
    assert(self == extrares)
    coroutine.yield(100)
  end)
  extrares = extra
  return table.unpack{10, x, 30}
end
check(foo, true, 10, "x", 30)
print("DONE", extrares)

local function foo2()
  local x <close> = func2close(coroutine.yield)
  return
end
check(foo2, false)

local function foo3()
  local x <close> = func2close(coroutine.yield)
  local y, z = 20, 30
  return x
end
check(foo3, false, "x")
