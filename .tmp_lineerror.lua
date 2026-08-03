local original_match = string.match
string.match = function(value, pattern, ...)
  local result = original_match(value, pattern, ...)
  print("MATCH", value, pattern, result)
  return result
end
local function lineerror(s, l)
  local err, msg = pcall(load(s))
  print(err, msg, string.match(msg, ":(%d+):"), l)
end
lineerror("local a\n for i=1,'a' do \n print(i) \n end", 2)
lineerror("\n local a \n for k,v in 3 \n do \n print(k) \n end", 3)
lineerror("\n\n for k,v in \n 3 \n do \n print(k) \n end", 4)
lineerror("function a.x.y ()\na=a+1\nend", 1)
lineerror([[a
(     -- <<
23)
]], 2)
lineerror([[local a = {x = 13}
a
.
x
(     -- <<
23
)]], 5)
