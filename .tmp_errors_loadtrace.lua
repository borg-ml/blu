local original_load = load
load = function(...)
  local source = select(1, ...)
  local loaded, message = original_load(...)
  if type(source) == "string" then
    print("LOAD", #source, loaded ~= nil, message)
  end
  return loaded, message
end
local chunk = assert(loadfile(".upstream/lua/lua-5.5.0-tests/errors.lua"))
local ok, message = pcall(chunk)
print("RESULT", ok, message)
