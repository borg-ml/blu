local function err(n)
  if type(n) ~= "number" then return n
  elseif n == 0 then return "END"
  else error(n - 1)
  end
end
local res, msg = xpcall(error, err, 170)
print("RESULT", res, msg, type(msg))
local res2, msg2 = xpcall(error, err, 300)
print("RESULT2", res2, msg2, type(msg2))
