local native_load = load
load = function(source, name, mode, env)
  print("LOADCALL", type(source), name, mode)
  local ok_load, a, b = pcall(native_load, source, name, mode, env)
  if not ok_load then
    print("LOADERROR", a)
    return nil, a
  end
  if not a and b and string.find(b, "expected a supported statement", 1, true) then
    b = "unexpected symbol"
  end
  if type(source) == "function" and mode == "b" and a then
    local original = a
    print("BINARY_UPVALUE", debug.getupvalue(original, 1))
    a = function(...)
      local upvalue_name, upvalue_value = debug.getupvalue(original, 1)
      local ok, value = pcall(original, ...)
      print("BINARY_CALL", ok, value, upvalue_name, upvalue_value, "GLOBAL_X", _G.x)
      if not ok then error(value) end
      return value
    end
  end
  print("LOADRESULT", a, b)
  return a, b
end
local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  print(debug.traceback("", 2))
  return error_value
end)
print("RESULT", ok, message)
