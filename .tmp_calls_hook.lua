local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
local n = 0
debug.sethook(function(_, line)
  n = n + 1
  if n < 500 then print("LINE", line) end
end, "l")
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  print(debug.traceback("", 2))
  return error_value
end)
print("RESULT", ok, message)
