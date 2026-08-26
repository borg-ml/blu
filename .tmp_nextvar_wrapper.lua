local builtin_assert = assert
function assert(condition, ...)
  if not condition then
    print("ASSERT-FAILED", ...)
  end
  return builtin_assert(condition, ...)
end
dofile(".upstream/lua/lua-5.4.8-tests/nextvar.lua")
