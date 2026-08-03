local C = 0
local l = debug.getinfo(1, "l").currentline + 1
local function auxy () C=C+1; auxy() end
local l1
local function g(x)
  l1 = debug.getinfo(x, "l").currentline + 2
  collectgarbage("stop")
  auxy()
  collectgarbage("restart")
end
local _, stackmsg = xpcall(g, debug.traceback, 1)
print("VALUES", l, l1)
print(stackmsg)
local stack = {}
for line in string.gmatch(stackmsg, "[^\n]*") do
  local curr = string.match(line, ":(%d+):")
  if curr then table.insert(stack, tonumber(curr)) end
end
for i, value in ipairs(stack) do
  print("STACK", i, value)
  if i > 30 then break end
end
