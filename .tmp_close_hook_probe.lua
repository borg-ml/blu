local trace = {}

local function func2close(f)
  return setmetatable({}, {__close = f})
end

local function hook(event)
  local info = debug.getinfo(2)
  trace[#trace + 1] = event .. " " .. tostring(info.name) .. " " .. tostring(info.namewhat)
end

local function foo(...)
  local x <close> = func2close(function()
    trace[#trace + 1] = "x"
  end)
  local y <close> = func2close(function()
    debug.sethook(hook, "r")
  end)
  return ...
end

local a, b, c = foo(10, 20, 30)
debug.sethook()
print("VALUES", a, b, c)
for i, value in ipairs(trace) do
  print("TRACE", i, value)
end
