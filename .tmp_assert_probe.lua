local function show(label, ...)
  local values = table.pack(...)
  print(label, values.n, type(values[1]), values[1], type(values[2]), values[2])
end
local function ecall(fn, ...)
  local ok, err = pcall(fn, ...)
  show("ECALL", ok, err)
  return err
end
show("MISSING", pcall(function() assert() end))
show("NIL", pcall(function() assert(nil) end))
show("FALSE", pcall(function() assert(false) end))
show("MSGNIL", pcall(function() assert(nil, nil) end))
show("MSG", pcall(function() assert(nil, "epic fail") end))
