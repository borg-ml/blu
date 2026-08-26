local n = 10000
for i = 1, n do
    local chunk = assert(load("return " .. i))
    assert(chunk() == i)
end
return true
