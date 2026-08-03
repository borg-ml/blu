local original_assert = assert
local original_find = string.find
string.find = function(value, pattern, ...)
  if pattern == "field 'a'" or (type(value) == "string" and original_find(value, "attempt to index", 1, true)) then
    print("FIND_TRACE", pattern, value)
  end
  return original_find(value, pattern, ...)
end
assert = function(value, ...)
  if not value then
    print("ASSERT_FAIL", ...)
    print(debug.traceback("", 2))
  end
  return original_assert(value, ...)
end
local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/errors.lua"))
local ok, message = pcall(chunk)
print("RESULT", ok, message)
