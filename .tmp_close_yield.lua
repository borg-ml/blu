local function func2close(f)
  return setmetatable({}, {__close = f})
end
local function new()
  return coroutine.create(function(what)
    local var <close> = func2close(function(t, err)
      if what == "yield" then
        coroutine.yield()
      elseif what == "error" then
        error(200)
      end
    end)
    string.gsub("a", "a", function()
      assert(not coroutine.isyieldable())
      assert(pcall(pcall, function()
        local ok, err = coroutine.close()
        print("inner", ok, err)
        error("unreachable")
      end))
    end)
  end)
end
local co = new()
local st, msg = coroutine.resume(co, "yield")
print(st, msg)
