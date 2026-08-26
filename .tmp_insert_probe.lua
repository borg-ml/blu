local ok, err = pcall(table.insert, {}, 2, 3, 4)
print(ok, err)
