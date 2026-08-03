local f = assert(load("a = 3"))
print("LOADED", debug.getupvalue(f, 1), debug.getupvalue(f, 2), debug.getupvalue(f, 3))
local c = {}
local g = assert(load("a = 3", nil, nil, c))
print("EXPLICIT", debug.getupvalue(g, 1), debug.getupvalue(g, 2), debug.getupvalue(g, 3))
