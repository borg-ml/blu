local source = "\nfunction foo ()\n  local "
for index = 1, 300 do
    source = source .. "v" .. index .. ", "
end
source = source .. "b\n"
local loaded, message = load(source)
print(loaded, message)
