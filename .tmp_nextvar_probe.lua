local co = coroutine.wrap(function (t)
  for k, v in pairs(t) do
    local k1 = next(t)
    if k ~= k1 then return "mismatch", type(k), type(k1), v end
    t[k] = nil
    local expected = (type(k) == "table" and k[1] or
                      type(k) == "function" and k() or
                      string.sub(k, 1, 1))
    if expected ~= v then return "value-mismatch", expected, v end
    coroutine.yield(v)
  end
end)
local t = {}
t[{1}] = 1
t[{2}] = 2
t[string.rep("a", 50)] = "a"
t[string.rep("b", 50)] = "b"
t[{3}] = 3
t[string.rep("c", 10)] = "c"
t[function () return 10 end] = 10
local count = 7
while co(t) do
  collectgarbage("collect")
  count = count - 1
end
return count, next(t)
