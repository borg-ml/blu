local file = ".upstream/lua/lua-5.4.8-tests/calls.lua"
local handle = assert(io.open(file, "rb"))
local source = handle:read("*a")
handle:close()
local needle = "a()  -- empty chunk"
assert(source:find(needle, 1, true))
source = source:gsub(needle, "print('BEFORE_EMPTY', a); a(); print('AFTER_EMPTY')", 1)
local chunk = assert(load(source, "calls-injected"))
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  print(debug.traceback("", 2))
  return error_value
end)
print("RESULT", ok, message)
