local count = 0
debug.sethook(function(_, line)
  count = count + 1
  if count < 10 then print("HOOK", line) end
end, "l")
local a = 1
a = a + 1
print("DONE", a)
