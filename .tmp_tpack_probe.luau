local unpack = string.unpack
local function check(label, fmt, data)
    local ok, err = pcall(unpack, fmt, data)
    print(label, ok, err)
end
for i = 9, 16 do
    check("little-" .. i, "<I" .. i, ("\0"):rep(i - 1) .. "\1")
    check("big-" .. i, ">i" .. i, "\1" .. ("\0"):rep(i - 1))
end
