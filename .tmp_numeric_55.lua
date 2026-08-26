local function checkerror (msg, f, ...)
  local s, err = pcall(f, ...)
  assert(not s and string.find(err, msg))
end

do
  local a
  a = 0; for i=1, 1, 1 do a=a+1 end; assert(a==1)
  a = 0; for i=10000, 1e4, -1 do a=a+1 end; assert(a==1)
  a = 0; for i=1, 0.99999, 1 do a=a+1 end; assert(a==0)
  a = 0; for i=9999, 1e4, -1 do a=a+1 end; assert(a==0)
  a = 0; for i=1, 0.99999, -1 do a=a+1 end; assert(a==1)
  a = 0; for i=0, 0.999999999, 0.1 do a=a+1 end; assert(a==10)
  a = 0; for i=1.0, 1, 1 do a=a+1 end; assert(a==1)
  a = 0; for i=-1.5, -1.5, 1 do a=a+1 end; assert(a==1)
  a = 0; for i=1e6, 1e6, -1 do a=a+1 end; assert(a==1)
  a = 0; for i=1.0, 0.99999, 1 do a=a+1 end; assert(a==0)
  a = 0; for i=99999, 1e5, -1.0 do a=a+1 end; assert(a==0)
  a = 0; for i=1.0, 0.99999, -1 do a=a+1 end; assert(a==1)
end

a = 0; for i="10","1","-2" do a=a+1 end; assert(a==5)

do
  local c
  local function checkfloat (i)
    assert(math.type(i) == "float")
    c = c + 1
  end
  c = 0; for i = 1.0, 10 do checkfloat(i) end; assert(c == 10)
  c = 0; for i = -1, -10, -1.0 do checkfloat(i) end; assert(c == 10)
  local function checkint (i)
    assert(math.type(i) == "integer")
    c = c + 1
  end
  local m = math.maxinteger
  c = 0; for i = m, m - 10, -1 do checkint(i) end; assert(c == 11)
  c = 0; for i = 1, 10.9 do checkint(i) end; assert(c == 10)
  c = 0; for i = 10, 0.001, -1 do checkint(i) end; assert(c == 10)
  c = 0; for i = 1, math.huge do if i > 10 then break end; checkint(i) end; assert(c == 10)
  c = 0; for i = -1, -math.huge, -1 do if i < -10 then break end; checkint(i) end; assert(c == 10)
end

do
  local function checkfor (from, to, step, t)
    local c = 0
    for i = from, to, step do
      c = c + 1
      assert(i == t[c])
    end
    assert(c == #t)
  end
  local maxi = math.maxinteger
  local mini = math.mininteger
  checkfor(mini, maxi, maxi, {mini, -1, maxi - 1})
  checkfor(mini, math.huge, maxi, {mini, -1, maxi - 1})
  checkfor(maxi, mini, mini, {maxi, -1})
  checkfor(maxi, mini, -maxi, {maxi, 0, -maxi})
  checkfor(maxi, -math.huge, mini, {maxi, -1})
  checkfor(maxi, mini, 1, {})
  checkfor(mini, maxi, -1, {})
  checkfor(maxi - 6, maxi, 3, {maxi - 6, maxi - 3, maxi})
  checkfor(mini + 4, mini, -2, {mini + 4, mini + 2, mini})
  local step = maxi // 10
  local c = mini
  for i = mini, maxi, step do assert(i == c); c = c + step end
  c = maxi
  for i = maxi, mini, -step do assert(i == c); c = c - step end
  checkfor(maxi, maxi, maxi, {maxi})
  checkfor(maxi, maxi, mini, {maxi})
  checkfor(mini, mini, maxi, {mini})
  checkfor(mini, mini, mini, {mini})
end

checkerror("'for' step is zero", function () for i = 1, 10, 0 do end end)
checkerror("'for' step is zero", function () for i = 1, -10, 0 do end end)
checkerror("'for' step is zero", function () for i = 1.0, -10, 0.0 do end end)
print("OK")
