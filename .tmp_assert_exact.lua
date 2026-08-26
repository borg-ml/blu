print("testing asserts")
if pcall(assert, false) or pcall(function() assert(false) end) then
  error('catastrophic assertion failure')
end
function ecall(fn, ...)
  local ok, err = pcall(fn, ...)
  assert(not ok)
  print("ERR", ok, type(err), err)
  return err:sub(err:find(": ") + 2, #err)
end
assert(ecall(function() assert() end) == "missing argument #1")
assert(ecall(function() assert(nil) end) == "assertion failed!")
assert(ecall(function() assert(false) end) == "assertion failed!")
assert(ecall(function() assert(nil, "epic fail") end) == "epic fail")
print("OK")
