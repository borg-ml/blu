local function loop(x, y, z)
  return 1 + loop(x, y, z)
end
local function checkerr(message, function_value, ...)
  local status, error_value = pcall(function_value, ...)
  local found = string.find(error_value, message)
  print("CHECKERR", status, error_value, found, type(found))
  assert(not status and found)
end
local top_ok, top_message = pcall(loop)
print("TOP", top_ok, top_message)
local res, message = xpcall(loop, function(value)
  print("HANDLER", value, string.find(value, "stack overflow"))
  checkerr("error handling", loop)
  print("SIN_AFTER", math.sin(0), math.sin(0) == 0)
  return 15
end)
print("RESULT", res, message, type(message))
