local function loop(x, y, z)
  return 1 + loop(x, y, z)
end
local top_ok, top_message = pcall(loop)
print("TOP", top_ok, top_message)
local res, message = xpcall(loop, function(value)
  print("HANDLER", value, string.find(value, "stack overflow"))
  local ok, nested = pcall(loop)
  print("NESTED", ok, nested)
  return 15
end)
print("RESULT", res, message, type(message))
