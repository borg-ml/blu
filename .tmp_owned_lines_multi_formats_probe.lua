local file = os.tmpname()
local function write(value)
  io.output(file); io.write(value):close()
end
write("0123456789\n")
print("start")
local ok, err = pcall(function()
  for a,b in io.lines(file, 1, 1) do
    print("iter1", tostring(a), tostring(b))
    if a == "\n" then assert(not b) else assert(tonumber(a) == tonumber(b)-1) end
  end
end)
print("iter1-result", tostring(ok), type(err), tostring(err))
ok, err = pcall(function()
  for a,b,c in io.lines(file, 1, 2, "a") do
    print("iter2", tostring(a), tostring(b), tostring(c))
    assert(a == "0" and b == "12" and c == "3456789\n")
  end
end)
print("iter2-result", tostring(ok), type(err), tostring(err))
ok, err = pcall(function()
  for a,b,c in io.lines(file, "a", 0, 1) do
    print("iter3", tostring(a), tostring(b), tostring(c))
    if a == "" then break end
    assert(a == "0123456789\n" and not b and not c)
  end
end)
print("iter3-result", tostring(ok), type(err), tostring(err))
collectgarbage()
write("00\n10\n20\n30\n40\n")
ok, err = pcall(function()
  for a,b in io.lines(file, "n", "n") do
    print("iter4", tostring(a), tostring(b))
    if a == 40 then assert(not b) else assert(a == b - 10) end
  end
end)
print("iter4-result", tostring(ok), type(err), tostring(err))
os.remove(file)
