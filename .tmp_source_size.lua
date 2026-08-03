for i = 50, 70 do
  for _, source in ipairs({"@" .. string.rep("x", i), string.rep("x", i - 10), "=" .. string.rep("x", i)}) do
    local _, message = load("x =", source)
    local prefix = string.match(message, "^([^:]*):")
    print(#source, #prefix, prefix, message)
  end
end
