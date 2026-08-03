local file = assert(io.open(".upstream/lua/lua-5.4.8-tests/errors.lua", "rb"))
local source = file:read("*a")
file:close()
local needle = [[checkmessage("local a = setmetatable({}, {__index = 10}).x",]]
local position = assert(string.find(source, needle, 1, true))
source = source:sub(1, position - 1)
  .. [[print("BEFORE_INDEX", doit("local a = setmetatable({}, {__index = 10}).x")); ]]
  .. source:sub(position)
local chunk, message = load(source, "@injected-errors.lua")
assert(chunk, message)
return chunk()
