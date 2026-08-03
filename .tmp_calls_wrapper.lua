local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  print(debug.traceback("", 2))
  return error_value
end)
print("RESULT", ok, message)
