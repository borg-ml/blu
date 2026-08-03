local track = {}
local function func2close(f)
  return setmetatable({}, {__close = f})
end
local function foo()
  local x <close> = func2close(function()
    track[#track + 1] = "x"
  end)
  track[#track + 1] = "foo"
end
foo()
print("TRACK", table.concat(track, ","))
