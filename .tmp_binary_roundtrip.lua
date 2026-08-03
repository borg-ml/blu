local source = "x = 1; return x"
local original = assert(load(source))
local dumped = string.dump(original)
print("DUMP", #dumped, dumped:sub(1, 4))
local restored, message = load(dumped, nil, "b")
print("RESTORED", restored, message)
if restored then
  local ok, result = pcall(restored)
  print("RESULT", ok, result, x)
end
