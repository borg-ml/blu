local source = "a = f(x" .. string.rep(",x", 260) .. ")"
local chunk, message = load(source)
print(chunk, message)
