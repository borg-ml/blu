local file = ".upstream/lua/lua-5.4.8-tests/calls.lua"
local handle = assert(io.open(file, "rb"))
local source = assert(handle:read("*a"))
handle:close()
source = source:gsub(
  "assert%(a%(%%) == 1 and _G%.x == 1%)",
  "print('BEFORE_BINARY', debug.getupvalue(a, 1), _G.x); local ok, value = pcall(a); print('AFTER_BINARY', ok, value, _G.x); assert(ok and value == 1 and _G.x == 1)"
)
local chunk, message = load(source, "@" .. file)
assert(chunk, message)
return chunk()
