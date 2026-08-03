local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
debug.sethook(function(_, line)
  if line >= 319 and line <= 330 then
    print("LINE", line)
    if line == 320 then
      local i = 1
      while true do
        local name, value = debug.getlocal(2, i)
        if not name then break end
        if name == "a" then
          local ok, result = pcall(value)
          print("CALL320", i, value, ok, result)
        end
        i = i + 1
      end
    end
    if line == 322 then
      local ok, value, message = pcall(load, function() return true end)
      print("LOADTRUE", ok, value, message)
    end
  end
end, "l")
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  return error_value
end)
print("RESULT", ok, message)
