local function probe(label, source)
  local f, err = load(source)
  print(label, f, err)
end
probe("unknown", "local x <XXX> = 10")
probe("const", "local xxx <const> = 20; xxx = 10")
probe("nested", [[
    local xx;
    local xxx <const> = 20;
    local yyy;
    local function foo ()
      local abc = xx + yyy + xxx;
      return function () return function () xxx = yyy end end
    end
  ]])
probe("malformed", "error")
probe("token", "while << do end")
probe("number", "1.000")
probe("long", "[[a]]")
probe("quoted", "'aa'")
probe("shift-right", "for >> do end")
probe("control", "a\1a = 1")
probe("byte", "\255a = 1")
probe("duplicate label", [[
  ::A:: a = 1
  ::A::
]])
probe("missing label", [[
  a = 1
  goto A
  do ::A:: end
]])
local function doit(source)
  local f, message = load(source)
  if not f then return message end
  local ok, value = pcall(f)
  return ok and nil or value
end
print("runtime arithmetic", doit("a = {} + 1"))
print("runtime bitwise", doit("a = {} | 1"))
print("runtime compare", doit("a = {} < 1"))
