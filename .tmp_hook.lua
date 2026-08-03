local co = coroutine.create(function()
  coroutine.yield(10)
  return 20
end)
local trace = {}
local function hook(event)
  trace[#trace + 1] = event
end
debug.sethook(co, hook, "clr")
repeat until not coroutine.resume(co)
print(#trace)
for i, event in ipairs(trace) do print(i, event) end
