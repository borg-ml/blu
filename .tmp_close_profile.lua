local co = coroutine.wrap(function()
  local value <close> = setmetatable({}, {__close = coroutine.yield})
  return "body"
end)
local yielded = table.pack(co())
local resumed = table.pack(co())
print(_VERSION, yielded.n, type(yielded[1]), yielded[2], resumed.n, resumed[1])
