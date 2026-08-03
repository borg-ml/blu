local source = "local a" .. string.rep(",a", 500) .. ";"
local loaded, message = load(source)
print(loaded, message)
