local file = assert(io.open(".upstream/luau/tests/conformance/pcall.luau", "r"))
local source = file:read("*a")
file:close()
local lines = {}
local line_number = 0
for line in source:gmatch("([^\\n]*)\\n") do
    line_number = line_number + 1
    if line_number <= 157 then
        lines[#lines + 1] = line
    end
end
source = table.concat(lines, "\\n")
local needle = "assert(recurse(calllimit - 2) == calllimit - 2)"
assert(source:find(needle, 1, true))
source = source:gsub(needle, "print('direct-result', recurse(calllimit - 2), calllimit - 2); " .. needle, 1)
local chunk, error_message = load(source, "pcall.luau")
assert(chunk, error_message)
chunk()
