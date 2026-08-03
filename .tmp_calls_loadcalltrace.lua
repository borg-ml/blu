local native_load = load
load = function(source, name, mode, env)
  local a, b = native_load(source, name, mode, env)
  if type(source) == "function" and a then
    local original = a
    a = function(...)
      print("CALLLOADED", name, mode)
      local first, second, third, fourth = original(...)
      print("CALLLOADED_RESULT", first, second, third, fourth)
      return first, second, third, fourth
    end
  end
  return a, b
end
local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  return error_value
end)
print("RESULT", ok, message)
