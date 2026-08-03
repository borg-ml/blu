for i = 999900, 1000000, 1 do
  local ok, message = pcall(function()
    return table.unpack({}, 1, i)
  end)
  if not ok then print(i, ok, message, type(message)); break end
end
