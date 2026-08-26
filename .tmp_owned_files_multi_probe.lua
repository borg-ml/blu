local file = os.tmpname()
do
  local f = assert(io.open(file, "w"))
  f:write[[
a line
another line
1234
3.45
one
two
three
]]
  assert(f:close())
  local g = assert(io.open(file, "r"))
  local l1, l2, n1, n2, dummy = g:read("l", "L", "n", "n")
  print("first", tostring(l1), tostring(l2), tostring(n1), tostring(n2), tostring(dummy))
  assert(l1 == "a line" and l2 == "another line\n" and n1 == 1234 and n2 == 3.45 and dummy == nil)
  assert(g:close())
  g = assert(io.open(file, "r"))
  l1, l2, n1, n2, local_c, l3, l4, dummy = g:read(7, "l", "n", "n", 1, "l", "l")
  print("second", tostring(l1), tostring(l2), tostring(n1), tostring(n2), tostring(local_c), tostring(l3), tostring(l4), tostring(dummy))
  assert(l1 == "a line\n" and l2 == "another line" and local_c == '\n' and n1 == 1234 and n2 == 3.45 and l3 == "one" and l4 == "two" and dummy == nil)
  assert(g:close())
  g = assert(io.open(file, "r"))
  l1, n1, n2, dummy = g:read("l", "n", "n", "l")
  print("third", tostring(l1), tostring(n1), tostring(n2), tostring(dummy))
  assert(l1 == "a line" and not n1)
  g:close()
end
assert(os.remove(file))
print("multi passed")
