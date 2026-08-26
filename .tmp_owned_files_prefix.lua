local debug = require "debug"
local maxint = math.maxinteger
assert(type(os.getenv"PATH") == "string")
assert(io.input(io.stdin) == io.stdin)
assert(not pcall(io.input, "non-existent-file"))
assert(io.output(io.stdout) == io.stdout)
local function testerr (msg, f, ...)
  local stat, err = pcall(f, ...)
  return (not stat and string.find(err, msg, 1, true))
end
local function checkerr (msg, f, ...)
  assert(testerr(msg, f, ...))
end
assert(not io.close(io.stdin) and
       not io.stdout:close() and
       not io.stderr:close())
checkerr("got no value", io.stdin.close)
print("prefix passed")
