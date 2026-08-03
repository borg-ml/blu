local chunk = assert(loadfile(".upstream/lua/lua-5.4.8-tests/calls.lua"))
debug.sethook(function(_, line)
  if line >= 345 and line <= 356 then print("REGION", line) end
  if line == 347 then
    print("AT347", _G, _G.x, type(a))
    local _, loaded = debug.getlocal(2, 19)
    print("LOADED", loaded)
    if loaded then
      local envprobe = load("return _ENV")
      local envok, envvalue = pcall(envprobe)
      print("ENVPROBE", envok, envvalue, envvalue == _G)
      local loadedok, loadedvalue = pcall(loaded)
      print("LOADEDPROBE", loadedok, loadedvalue, _G.x)
      local up = 1
      while true do
        local name, value = debug.getupvalue(loaded, up)
        if not name then break end
        print("UP", up, name, value)
        up = up + 1
      end
    end
    local i = 1
    while true do
      local name, value = debug.getlocal(2, i)
      if not name then break end
      print("LOCAL", i, name, value)
      i = i + 1
    end
  end
end, "l")
local ok, message = xpcall(chunk, function(error_value)
  print("ERROR", error_value)
  print(debug.traceback("", 2))
  return error_value
end)
print("RESULT", ok, message)
