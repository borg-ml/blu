local original_pcall = pcall
pcall = function(function_value, ...)
  local results = {original_pcall(function_value, ...)}
  if not results[1] then print("PCALL_ERROR", results[2]) end
  return table.unpack(results)
end
local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/errors.lua"))
local ok, message = original_pcall(chunk)
print("RESULT", ok, message)
