local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
debug.sethook(function(_, line)
  if line == 320 then
    local i = 1
    while true do
      local name, value = debug.getlocal(2, i)
      if not name then break end
      if name == "a" then
        print("A", i, value)
        for attempt = 1, 3 do
          local ok, result = pcall(value)
          print("APCALL", attempt, ok, result)
        end
      end
      i = i + 1
    end
  end
end, "l")
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  return error_value
end)
print("RESULT", ok, message)
