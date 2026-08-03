local code = {"local x = {"}
for i = 1, 257 do code[#code + 1] = i .. ".1," end
code[#code + 1] = "}; return (1 ~ (2 or 3))"
code = table.concat(code)
local f, message = load(code)
print("LOAD", f, message)
if f then print("VALUE", f()) end
