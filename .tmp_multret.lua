local function report(label, ...)
  print(label, select("#", ...), select(1, ...), select(2, ...))
end
local f, e = load("local x <XXX> = 10")
report("load locals", f, e)
report("select load", select(2, load("local x <XXX> = 10")))
print("direct", select("#", load("local x <XXX> = 10")))
local _, error_message = load("local x <XXX> = 10")
print("find direct", string.find(error_message, "unknown attribute 'XXX'"))
print("find selected", string.find(select(2, load("local x <XXX> = 10")), "unknown attribute 'XXX'"))
local function check(s, pattern)
  local f, message = load(s)
  print("check", f, message, pattern, string.find(message, pattern))
end
check("local xxx <const> = 20; xxx = 10", ":1: attempt to assign to const variable 'xxx'")
check([[local xx;
local xxx <const> = 20;
local yyy;
local function foo ()
  local abc = xx + yyy + xxx;
  return function () return function () xxx = yyy end end
end]], ":6: attempt to assign to const variable 'xxx'")
check([[local x <close> = nil
x = io.open()]], ":2: attempt to assign to const variable 'x'")
