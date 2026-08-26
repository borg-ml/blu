print("start")
local function checkresults(e, ...)
    local t = table.pack(...)
    print("check", t.n, t[1], t[2], t[3])
end
local function checkerror(...)
    local t = table.pack(...)
    print("error", t.n, t[1], t[2])
end
function stackinfinite() return stackinfinite() end
checkerror(pcall(stackinfinite))
function stackover() return pcall(stackover) end
local res = {pcall(stackover)}
print("stackover", #res, res[1], res[#res])
local calllimit = 20000
function recurse(n)
    return n <= 1 and 1 or recurse(n-1) + 1
end
local direct = recurse(calllimit - 2)
print("top-direct", direct, direct == calllimit - 2)
checkresults({ true, calllimit - 3 }, pcall(recurse, calllimit - 3))
checkerror(pcall(recurse, calllimit - 2))
