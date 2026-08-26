local original_setupvalue = debug.setupvalue
debug.setupvalue = function(function_value, index, value, ...)
  local result = original_setupvalue(function_value, index, value, ...)
  if result == "_ENV" then
    original_setupvalue(function_value, 1, nil)
    print("SETUP_ENV", debug.getupvalue(function_value, 1), debug.getupvalue(function_value, 2), _G.XX, type(value))
  end
  return result
end
local original_assert = assert
assert = function(value, ...)
  if not value then
    local info = debug.getinfo(2, "Sl")
    print("ASSERT_FAIL", info and info.currentline or "?", ...)
  end
  return original_assert(value, ...)
end
local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
local ok, message = pcall(chunk)
print("DONE", ok, message)
