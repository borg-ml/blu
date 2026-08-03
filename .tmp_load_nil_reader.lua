local a, b = load(function() return nil end)
print("LOAD", a, b)
print("TYPE", type(a))
if a then print("CALL", a()) end
if a then print("CALL2", pcall(a)) end
