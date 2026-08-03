local track = {}
local function func2close(f)
  return setmetatable({}, {__close = f})
end
local function foo()
  local x0 <close> = func2close(function(_, msg)
    assert(msg == 202)
    track[#track + 1] = "x0"
  end)
  local x <close> = func2close(function()
    local xx <close> = func2close(function(_, msg)
      assert(msg == 101)
      track[#track + 1] = "xx"
      error(202)
    end)
    track[#track + 1] = "x"
    error(101)
  end)
  track[#track + 1] = "foo"
  return 20, 30, 40
end
local st, msg = pcall(foo)
print("RESULT", st, msg, "TRACK", table.concat(track, ","))
