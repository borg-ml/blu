local scope_count = -1
do
  local value <close> = setmetatable({}, {__close = function(_, ...)
    scope_count = select("#", ...)
    print("CLOSE_COUNT", _VERSION, scope_count)
  end})
end
local error_count = -1
local ok = pcall(function()
  local value <close> = setmetatable({}, {__close = function(_, ...)
    error_count = select("#", ...)
    print("ERROR_COUNT", _VERSION, error_count)
  end})
  error("boom")
end)
print("RESULT", _VERSION, scope_count, error_count, ok)
local f = function(_, ...)
  return select("#", ...)
end
print("DIRECT", _VERSION, f({}, nil), f({}, 123))
