local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
debug.sethook(function(_, line)
  if line == 403 then
    print("AT_LINE", line)
    for index = 1, 40 do
      local name, value = debug.getlocal(2, index)
      if not name then break end
      if name == "x" or name == "XX" or name == "_ENV" then
        print("LOCAL", index, name, type(value), value)
      end
      if name == "x" and type(value) == "function" then
        print("X_UPVALUES", debug.getupvalue(value, 1), debug.getupvalue(value, 2), debug.getupvalue(value, 3))
        local ok, result = pcall(value)
        print("XCALL_HOOK", ok, result, _G.XX, XX)
      end
    end
  end
end, "l")
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  return error_value
end)
print("RESULT", ok, message)
