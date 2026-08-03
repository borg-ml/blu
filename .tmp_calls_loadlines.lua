local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
debug.sethook(function(_, line)
  if line >= 295 and line <= 360 then print("LINE", line) end
end, "l")
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  return error_value
end)
print("RESULT", ok, message)
