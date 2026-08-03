local original_assert = assert
local original_pcall = pcall
local original_setmetatable = setmetatable
setmetatable = function(value, metatable)
    local marker = metatable and metatable.__index
    if marker == 10 then print("TARGET_SETMETATABLE") end
    return original_setmetatable(value, metatable)
end
pcall = function(function_value, ...)
    local ok, value = original_pcall(function_value, ...)
    if not ok then print("PCALL_ERROR", value) end
    return ok, value
end
local original_find = string.find
local original_load = load
load = function(source, ...)
    local function_value, message = original_load(source, ...)
    if type(source) == "string" and original_find(source, "__index = 10", 1, true) then
        print("TARGET_LOAD", function_value, message)
        if function_value then
            return function(...)
                local ok, value = original_pcall(function_value, ...)
                print("TARGET_RUN", ok, value)
                if ok then return value end
                error(value)
            end
        end
    end
    return function_value, message
end
string.find = function(value, pattern, ...)
    print("FIND", pattern, value)
    return original_find(value, pattern, ...)
end
assert = function(value, ...)
    if not value then
        print("ASSERT_FAIL")
        print(debug.traceback("", 2))
        for level = 2, 6 do
            local parts = {}
            for index = 1, 12 do
                local success, name, local_value = pcall(debug.getlocal, level, index)
                if not success then break end
                if not name then break end
                parts[#parts + 1] = name .. "=" .. tostring(local_value)
            end
            if #parts > 0 then print(level, table.concat(parts, ";")) end
        end
    end
    return original_assert(value, ...)
end
local chunk, load_message = loadfile(".upstream/lua/lua-5.4.8-tests/errors.lua")
print("LOADFILE", chunk, load_message)
local ok, message = pcall(chunk)
print("CHUNK", ok, message)
