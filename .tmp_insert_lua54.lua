local status, message = pcall(table.insert, {}, 2, 3, 4)
print(status, message, type(message), string.find(message, "wrong number of arguments"))
