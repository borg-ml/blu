local file = os.tmpname()
os.remove(file)
local ok, err = pcall(dofile, file)
print("dofile", tostring(ok), type(err), tostring(err))
local function testerr (msg, f, ...)
  local stat, value = pcall(f, ...)
  return (not stat and string.find(value, msg, 1, true))
end
print("testerr-empty", testerr("", dofile, file))
