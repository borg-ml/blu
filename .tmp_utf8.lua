local utf8 = require 'utf8'
local s = utf8.char(0x200000, 0x3ffffff)
print("ROUND", #s, string.byte(s, 1, #s))
print("CP", utf8.codepoint(s, 1, 6, true), utf8.codepoint(s, 7, 12, true))
local function probe(s, t)
  local justone = "^" .. utf8.charpattern .. "$"
  local l = utf8.len(s, 1, -1, nil)
  print("S", s, l)
  print("pattern", utf8.charpattern, justone)
  assert(#t == l and #string.gsub(s, "[\x80-\xBF]", "") == l)
  assert(utf8.char(table.unpack(t)) == s)
  assert(utf8.offset(s, 0) == 1)
  local ts = {"return '"}
  for i = 1, #t do ts[i + 1] = string.format("\\u{%x}", t[i]) end
  ts[#t + 2] = "'"
  assert(assert(load(table.concat(ts)))() == s)
  local t1 = {utf8.codepoint(s, 1, -1, nil)}
  assert(#t == #t1)
  for i = 1, #t do assert(t[i] == t1[i]) end
  for i=1,l do
    local pi=utf8.offset(s,i)
    local pi1=utf8.offset(s,2,pi)
    print(i,pi,pi1,string.sub(s,pi,pi1-1),string.find(string.sub(s,pi,pi1-1),justone))
  end
end
probe("hello World", {string.byte("hello World", 1, -1)})
probe("汉字/漢字", {27721, 23383, 47, 28450, 23383})
