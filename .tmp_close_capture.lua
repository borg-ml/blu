local X = true
local handler = function(self, err) X = false end
local co = coroutine.create(function()
  local x <close> = setmetatable({}, {__close = handler})
  coroutine.yield()
end)
coroutine.resume(co)
print("before", X)
print("close", coroutine.close(co))
print("after", X)
