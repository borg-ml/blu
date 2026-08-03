local original_find = string.find
local original_assert = assert
local original_match = string.match
local original_load = load
local original_xpcall = xpcall
local original_traceback = debug.traceback
load = function(...)
  local loaded, message = original_load(...)
  local source = select(1, ...)
  if type(source) == "string" and original_find(source, "function foo", 1, true) then
    print("LOAD_LOCAL", loaded, message)
  end
  if type(source) == "string" and #source > 1000 then
    print("LOAD_LARGE", #source, loaded ~= nil, message)
  end
  if type(source) == "string" and #source > 100 and #source < 2000 then
    print("LOAD_PROBE", #source, loaded ~= nil, message)
  end
  return loaded, message
end
xpcall = function(target, handler, ...)
  local results = {original_xpcall(target, handler, ...)}
  print("XPCALL_RESULT", results[1], results[2], type(results[2]))
  return table.unpack(results)
end
debug.traceback = function(...)
  local result = original_traceback(...)
  print("TRACEBACK_RESULT", result, type(result))
  return result
end
string.find = function(value, pattern, ...)
  local result = original_find(value, pattern, ...)
  print("FIND", pattern, value, result)
  return result
end
string.match = function(value, pattern, ...)
  local result = original_match(value, pattern, ...)
  if pattern == ":(%d+):" then print("LINE", value, result) end
  return result
end
assert = function(value, ...)
  if not value then print("ASSERT_VALUE", value, ...); print("ASSERT", debug.traceback("", 2)) end
  return original_assert(value, ...)
end
local chunk = assert(loadfile(".upstream/lua/lua-5.5.0-tests/errors.lua"))
local ok, message = pcall(chunk)
print("RESULT", ok, message)
