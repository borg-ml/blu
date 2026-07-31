#![forbid(unsafe_code)]

use blu_compiler::{Compiler as SourceCompiler, owned::OwnedCompiler};
use blu_core::{
    CompilerId, CompilerIdentity, IdentityLimits, SemanticProfile, SourceFile, SourceId,
    SourceLimits,
};
use blu_lang::Engine;
use blu_runtime::{
    Dialect, Value, Vm,
    bytecode::{LoadLimits, blu::BluLimits, disassemble, load},
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const PINNED_REVISION: &str = "f8ca77acdcb50241e3da21af663f8ef97b4b5ce4";
const LUA_REFERENCES: [(&str, &str); 5] = [
    ("5.1.5", "Lua 5.1"),
    ("5.2.4", "Lua 5.2"),
    ("5.3.6", "Lua 5.3"),
    ("5.4.8", "Lua 5.4"),
    ("5.5.0", "Lua 5.5"),
];
const PORTABLE_EXPECTED: &str = "14\nlu";
const PORTABLE_SOURCE: &str = r#"
local values = { 3, 1, 4 }
values[2] = values[1] + values[3]

local function sum(items)
    local total = 0
    for index = 1, #items do
        total = total + items[index]
    end
    return total
end

print(sum(values))
print(string.sub("blu", 2, 3))
"#;
const SCALAR_CASES: [(&str, &str); 12] = [
    ("addition", "1 + 2"),
    ("precedence", "(9 - 4) * 3"),
    ("division", "7 / 2"),
    ("floor division", "-7 // 3"),
    ("modulo", "17 % 5"),
    ("power", "2 ^ 8"),
    ("string", "\"blu\""),
    ("string length", "#(\"borg\")"),
    ("not", "not false"),
    ("and", "true and 4"),
    ("or", "false or 9"),
    ("comparison", "3 < 4"),
];
const OWNED_CALLBACK_SOURCE: &str = r#"
local values = { 2, 1 }
table.sort(values, function(left, right) return left > right end)
local transformed, count = string.gsub("a1b2", "(%a)(%d)", function(letter, digit)
    return digit .. letter
end)
local iterator = string.gmatch("a1b2", "(%a)(%d)")
local first, first_digit = iterator()
local second, second_digit = iterator()
local done = iterator()
return values[1] == 2 and values[2] == 1 and transformed == "1a2b" and count == 2
    and type(iterator) == "function" and first == "a" and first_digit == "1"
    and second == "b" and second_digit == "2" and done == nil
"#;
const OWNED_CALLBACK_REFERENCE_SOURCE: &str = r#"
local values = { 2, 1 }
table.sort(values, function(left, right) return left > right end)
local transformed, count = string.gsub("a1b2", "(%a)(%d)", function(letter, digit)
    return digit .. letter
end)
local iterator = string.gmatch("a1b2", "(%a)(%d)")
local first, first_digit = iterator()
local second, second_digit = iterator()
local done = iterator()
local result = values[1] == 2 and values[2] == 1 and transformed == "1a2b" and count == 2
    and type(iterator) == "function" and first == "a" and first_digit == "1"
    and second == "b" and second_digit == "2" and done == nil
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_SOURCE: &str = r#"
local values = {}
values[1] = 3
values.answer = 4
return values[1] + values.answer
"#;
const TABLE_REFERENCE_SOURCE: &str = r#"
local values = {}
values[1] = 3
values.answer = 4
print(type(values[1] + values.answer) .. ":" .. tostring(values[1] + values.answer))
"#;
const LOOP_SOURCE: &str = r#"
local total = 0
for index = 1, 5 do
    total += index
end
return total
"#;
const LOOP_REFERENCE_SOURCE: &str = r#"
local total = 0
for index = 1, 5 do
    total += index
end
print(type(total) .. ":" .. tostring(total))
"#;
const FUNCTION_SOURCE: &str = r#"
local function add(left, right)
    return left + right
end
return add(3, 4)
"#;
const FUNCTION_REFERENCE_SOURCE: &str = r#"
local function add(left, right)
    return left + right
end
local value = add(3, 4)
print(type(value) .. ":" .. tostring(value))
"#;
const CAPTURE_SOURCE: &str = r#"
local value = 2
local function bump()
    value += 3
    return value
end
bump()
return bump()
"#;
const CAPTURE_REFERENCE_SOURCE: &str = r#"
local value = 2
local function bump()
    value += 3
    return value
end
bump()
local result = bump()
print(type(result) .. ":" .. tostring(result))
"#;
const NESTED_CAPTURE_SOURCE: &str = r#"
local value = 4
local function outer()
    local function inner()
        return value
    end
    return inner()
end
return outer()
"#;
const NESTED_CAPTURE_REFERENCE_SOURCE: &str = r#"
local value = 4
local function outer()
    local function inner()
        return value
    end
    return inner()
end
local result = outer()
print(type(result) .. ":" .. tostring(result))
"#;
const PARENT_CAPTURE_SOURCE: &str = r#"
local value = 1
local function update()
    value = 9
end
update()
return value
"#;
const PARENT_CAPTURE_REFERENCE_SOURCE: &str = r#"
local value = 1
local function update()
    value = 9
end
update()
print(type(value) .. ":" .. tostring(value))
"#;
const VARARGS_SOURCE: &str = r#"
local function sum(first, ...)
    local second, third = ...
    return first + second + third
end
return sum(1, 2, 3)
"#;
const VARARGS_REFERENCE_SOURCE: &str = r#"
local function sum(first, ...)
    local second, third = ...
    return first + second + third
end
local result = sum(1, 2, 3)
print(type(result) .. ":" .. tostring(result))
"#;
const MULTRET_SOURCE: &str = r#"
local function pair()
    return 2, 3
end
local function sum(left, right)
    return left + right
end
return sum(pair())
"#;
const MULTRET_REFERENCE_SOURCE: &str = r#"
local function pair()
    return 2, 3
end
local function sum(left, right)
    return left + right
end
local result = sum(pair())
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_LITERAL_SOURCE: &str = r#"
local values = { 3, 4, alpha = 2, beta = "x" }
return values[1] + values[2] + values.alpha + #values.beta
"#;
const TABLE_LITERAL_REFERENCE_SOURCE: &str = r#"
local values = { 3, 4, alpha = 2, beta = "x" }
local result = values[1] + values[2] + values.alpha + #values.beta
print(type(result) .. ":" .. tostring(result))
"#;
const GENERIC_FOR_SOURCE: &str = r#"
local array = { 10, 20, 30 }
local object = { alpha = 4, beta = 5 }
local result = 0
for index, value in ipairs(array) do
    result += index + value
end
for key, value in pairs(object) do
    result += value
end
return result
"#;
const GENERIC_FOR_REFERENCE_SOURCE: &str = r#"
local array = { 10, 20, 30 }
local object = { alpha = 4, beta = 5 }
local result = 0
for index, value in ipairs(array) do
    result += index + value
end
for key, value in pairs(object) do
    result += value
end
print(type(result) .. ":" .. tostring(result))
"#;
const METHOD_CALL_SOURCE: &str = r#"
return ("abcdef"):sub(2, 4)
"#;
const METHOD_CALL_REFERENCE_SOURCE: &str = r#"
local result = ("abcdef"):sub(2, 4)
print(type(result) .. ":" .. tostring(result))
"#;
const DIRECT_ITERATION_SOURCE: &str = r#"
local values = { alpha = 3, beta = 7 }
local result = 0
for key, value in values do
    result += value
end
return result
"#;
const DIRECT_ITERATION_REFERENCE_SOURCE: &str = r#"
local values = { alpha = 3, beta = 7 }
local result = 0
for key, value in values do
    result += value
end
print(type(result) .. ":" .. tostring(result))
"#;
const BASE_LIBRARY_SOURCE: &str = r#"
return type(3) .. ":" .. tostring(3)
"#;
const BASE_LIBRARY_REFERENCE_SOURCE: &str = r#"
local result = type(3) .. ":" .. tostring(3)
print(type(result) .. ":" .. tostring(result))
"#;
const PACKAGE_SOURCE: &str = r#"
return type(package.loaded) .. ":" .. type(package.preload)
"#;
const PACKAGE_REFERENCE_SOURCE: &str = r#"
local result = type(package.loaded) .. ":" .. type(package.preload)
print(type(result) .. ":" .. tostring(result))
"#;
const PACKAGE_PRELOAD_SOURCE: &str = r#"
local calls = 0
package.preload.answer = function(name)
    calls = calls + 1
    return { name = name, value = 42 }
end
package.preload.empty = function() end
local first = require("answer")
local second = require("answer")
return first.name == "answer"
    and first.value == 42
    and first == second
    and first == package.loaded.answer
    and calls == 1
    and require("empty") == true
    and package.loaded.empty == true
"#;
const PACKAGE_PRELOAD_REFERENCE_SOURCE: &str = r#"
local calls = 0
package.preload.answer = function(name)
    calls = calls + 1
    return { name = name, value = 42 }
end
package.preload.empty = function() end
local first = require("answer")
local second = require("answer")
local result = first.name == "answer"
    and first.value == 42
    and first == second
    and first == package.loaded.answer
    and calls == 1
    and require("empty") == true
    and package.loaded.empty == true
print(type(result) .. ":" .. tostring(result))
"#;
const METATABLE_SOURCE: &str = r#"
local prototype = { answer = 42 }
local object = setmetatable({}, { __index = prototype })
return object.answer
"#;
const METATABLE_REFERENCE_SOURCE: &str = r#"
local prototype = { answer = 42 }
local object = setmetatable({}, { __index = prototype })
local result = object.answer
print(type(result) .. ":" .. tostring(result))
"#;
const CALLABLE_INDEX_SOURCE: &str = r#"
local object = setmetatable({ base = 5 }, {
    __index = function(self, key)
        return self.base + #key
    end,
})
return object.abc
"#;
const CALLABLE_INDEX_REFERENCE_SOURCE: &str = r#"
local object = setmetatable({ base = 5 }, {
    __index = function(self, key)
        return self.base + #key
    end,
})
local result = object.abc
print(type(result) .. ":" .. tostring(result))
"#;
const NEWINDEX_SOURCE: &str = r#"
local function_target = {}
local function_object = setmetatable({}, {
    __newindex = function(self, key, value)
        function_target[key] = value * 2
    end,
})
function_object.answer = 9

local table_target = {}
local table_object = setmetatable({}, { __newindex = table_target })
table_object.extra = 4
return function_target.answer + table_target.extra
"#;
const NEWINDEX_REFERENCE_SOURCE: &str = r#"
local function_target = {}
local function_object = setmetatable({}, {
    __newindex = function(self, key, value)
        function_target[key] = value * 2
    end,
})
function_object.answer = 9

local table_target = {}
local table_object = setmetatable({}, { __newindex = table_target })
table_object.extra = 4
local result = function_target.answer + table_target.extra
print(type(result) .. ":" .. tostring(result))
"#;
const ARITHMETIC_METAMETHOD_SOURCE: &str = r#"
local value = setmetatable({}, {
    __add = function(left, right) return 11 end,
    __mul = function(left, right) return 12 end,
    __idiv = function(left, right) return 13 end,
})
return (value + 3) + (3 * value) + (value // 2)
"#;
const ARITHMETIC_METAMETHOD_REFERENCE_SOURCE: &str = r#"
local value = setmetatable({}, {
    __add = function(left, right) return 11 end,
    __mul = function(left, right) return 12 end,
    __idiv = function(left, right) return 13 end,
})
local result = (value + 3) + (3 * value) + (value // 2)
print(type(result) .. ":" .. tostring(result))
"#;
const UNARY_METAMETHOD_SOURCE: &str = r#"
local value = setmetatable({}, {
    __unm = function(self) return 20 end,
    __len = function(self) return 3 end,
})
return (-value) + #value
"#;
const UNARY_METAMETHOD_REFERENCE_SOURCE: &str = r#"
local value = setmetatable({}, {
    __unm = function(self) return 20 end,
    __len = function(self) return 3 end,
})
local result = (-value) + #value
print(type(result) .. ":" .. tostring(result))
"#;
const COMPARISON_METAMETHOD_SOURCE: &str = r#"
local metatable = {
    __eq = function(left, right) return true end,
    __lt = function(left, right) return left.rank < right.rank end,
}
local left = setmetatable({ rank = 1 }, metatable)
local right = setmetatable({ rank = 2 }, metatable)
return left == right and left < right and left <= right and not (right < left)
"#;
const COMPARISON_METAMETHOD_REFERENCE_SOURCE: &str = r#"
local metatable = {
    __eq = function(left, right) return true end,
    __lt = function(left, right) return left.rank < right.rank end,
}
local left = setmetatable({ rank = 1 }, metatable)
local right = setmetatable({ rank = 2 }, metatable)
local result = left == right and left < right and left <= right and not (right < left)
print(type(result) .. ":" .. tostring(result))
"#;
const CALL_AND_CONCAT_METAMETHOD_SOURCE: &str = r#"
local callable = setmetatable({ factor = 3 }, {
    __call = function(self, value) return self.factor * value end,
})
local metatable = {
    __concat = function(left, right) return left.text .. right.text end,
}
local left = setmetatable({ text = "bl" }, metatable)
local right = setmetatable({ text = "u" }, metatable)
return callable(4) + #(left .. right)
"#;
const CALL_AND_CONCAT_METAMETHOD_REFERENCE_SOURCE: &str = r#"
local callable = setmetatable({ factor = 3 }, {
    __call = function(self, value) return self.factor * value end,
})
local metatable = {
    __concat = function(left, right) return left.text .. right.text end,
}
local left = setmetatable({ text = "bl" }, metatable)
local right = setmetatable({ text = "u" }, metatable)
local result = callable(4) + #(left .. right)
print(type(result) .. ":" .. tostring(result))
"#;
const RAW_BASE_SOURCE: &str = r#"
local object = setmetatable({}, {
    __index = function() return 99 end,
    __newindex = function() error("must not run") end,
    __len = function() return 99 end,
})
rawset(object, "answer", 3)
return rawget(object, "answer") + rawlen(object) + (rawequal(object, object) and 1 or 0)
"#;
const RAW_BASE_REFERENCE_SOURCE: &str = r#"
local object = setmetatable({}, {
    __index = function() return 99 end,
    __newindex = function() error("must not run") end,
    __len = function() return 99 end,
})
rawset(object, "answer", 3)
local result = rawget(object, "answer") + rawlen(object) + (rawequal(object, object) and 1 or 0)
print(type(result) .. ":" .. tostring(result))
"#;
const ASSERT_SELECT_SOURCE: &str = r##"
local result = select("#", 1, 2, 3) + select(2, 10, 20, 30)
return assert(result == 23, "unexpected select result") and result
"##;
const ASSERT_SELECT_REFERENCE_SOURCE: &str = r##"
local result = select("#", 1, 2, 3) + select(2, 10, 20, 30)
result = assert(result == 23, "unexpected select result") and result
print(type(result) .. ":" .. tostring(result))
"##;
const PROTECTED_CALL_SOURCE: &str = r#"
local ok, first, second = pcall(function(value)
    return value, value + 1
end, 4)
local failed, message = pcall(function()
    error("boom")
end)
return ok and not failed and first == 4 and second == 5 and type(message) == "string"
"#;
const PROTECTED_CALL_REFERENCE_SOURCE: &str = r#"
local ok, first, second = pcall(function(value)
    return value, value + 1
end, 4)
local failed, message = pcall(function()
    error("boom")
end)
local result = ok and not failed and first == 4 and second == 5 and type(message) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const CLOSE_ERROR_SOURCE: &str = r#"
local error_type = ""
local ok = pcall(function()
    local resource <close> = setmetatable({}, {
        __close = function(value, err)
            error_type = type(err)
        end,
    })
    error("body")
end)
return tostring(ok) .. ":" .. error_type
"#;
const CLOSE_ERROR_REFERENCE_SOURCE: &str = r#"
local error_type = ""
local ok = pcall(function()
    local resource <close> = setmetatable({}, {
        __close = function(value, err)
            error_type = type(err)
        end,
    })
    error("body")
end)
local result = tostring(ok) .. ":" .. error_type
print(type(result) .. ":" .. tostring(result))
"#;
const CLOSE_REVERSE_SOURCE: &str = r#"
local order = ""
local ok = pcall(function()
    local first <close> = setmetatable({}, {
        __close = function()
            order = order .. "a"
            error("close")
        end,
    })
    local second <close> = setmetatable({}, {
        __close = function()
            order = order .. "b"
        end,
    })
    error("body")
end)
return tostring(ok) .. ":" .. order
"#;
const CLOSE_REVERSE_REFERENCE_SOURCE: &str = r#"
local order = ""
local ok = pcall(function()
    local first <close> = setmetatable({}, {
        __close = function()
            order = order .. "a"
            error("close")
        end,
    })
    local second <close> = setmetatable({}, {
        __close = function()
            order = order .. "b"
        end,
    })
    error("body")
end)
local result = tostring(ok) .. ":" .. order
print(type(result) .. ":" .. tostring(result))
"#;
const CLOSE_YIELD_SOURCE: &str = r#"
local wrapped = coroutine.wrap(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            return coroutine.yield("closing")
        end,
    })
    return "done"
end)
local first = wrapped()
local second = wrapped("resumed")
return first .. ":" .. second
"#;
const CLOSE_YIELD_REFERENCE_SOURCE: &str = r#"
local wrapped = coroutine.wrap(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            return coroutine.yield("closing")
        end,
    })
    return "done"
end)
local first = wrapped()
local second = wrapped("resumed")
local result = first .. ":" .. second
print(type(result) .. ":" .. tostring(result))
"#;
const ENVIRONMENT_SOURCE: &str = r#"
local _ENV = { a = 1, b = 2, type = type, tostring = tostring, print = print }
a, b = b, a
function answer()
    return a + b
end
return answer() .. ":" .. a .. ":" .. b
"#;
const ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
local _ENV = { a = 1, b = 2, type = type, tostring = tostring, print = print }
a, b = b, a
function answer()
    return a + b
end
local result = answer() .. ":" .. a .. ":" .. b
print(type(result) .. ":" .. tostring(result))
"#;
const DEFAULT_ENVIRONMENT_SOURCE: &str = r#"
answer = 40
local function make()
    local function read()
        answer = answer + 2
        return answer
    end
    return read
end
local read = make()
read()
return answer .. ":" .. _ENV.answer
"#;
const DEFAULT_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
answer = 40
local function make()
    local function read()
        answer = answer + 2
        return answer
    end
    return read
end
local read = make()
read()
local result = answer .. ":" .. _ENV.answer
print(type(result) .. ":" .. tostring(result))
"#;
const LOAD_ENVIRONMENT_SOURCE: &str = r#"
answer = 39
local default_loaded = load("answer = answer + 1; return answer")
local default_result = default_loaded()
local environment = { answer = 40 }
local loaded = load("answer = answer + 1; return answer", "chunk", "t", environment)
local first = loaded()
local second = loaded()
return default_result == 40 and answer == 40 and first == 41 and second == 42
    and environment.answer == 42
"#;
const LOAD_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
answer = 39
local default_loaded = load("answer = answer + 1; return answer")
local default_result = default_loaded()
local environment = { answer = 40 }
local loaded = load("answer = answer + 1; return answer", "chunk", "t", environment)
local first = loaded()
local second = loaded()
local result = default_result == 40 and answer == 40 and first == 41 and second == 42
    and environment.answer == 42
print(type(result) .. ":" .. tostring(result))
"#;
const LOAD_READER_SOURCE: &str = r#"
    local chunks = { "return 40", " + 2" }
local index = 0
local loaded, message = load(function()
    index = index + 1
    return chunks[index]
end)
return loaded ~= nil and message == nil and loaded() == 42 and index == 3
"#;
const LOAD_READER_REFERENCE_SOURCE: &str = r#"
local chunks = { "return 40", " + 2" }
local index = 0
local loaded, message = load(function()
    index = index + 1
    return chunks[index]
end)
local result = loaded ~= nil and message == nil and loaded() == 42 and index == 3
print(type(result) .. ":" .. tostring(result))
"#;
const LUA51_ENVIRONMENT_SOURCE: &str = r#"
local environment = { answer = 40 }
local function read()
    return answer
end
setfenv(read, environment)
local loaded = loadstring("answer = answer + 1; return answer")
setfenv(loaded, environment)
local first = loaded()
local second = loaded()
return first == 41 and second == 42 and environment.answer == 42
    and getfenv(read) == environment and getfenv(loaded) == environment
"#;
const LUA51_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
local environment = { answer = 40 }
local function read()
    return answer
end
setfenv(read, environment)
local loaded = loadstring("answer = answer + 1; return answer")
setfenv(loaded, environment)
local first = loaded()
local second = loaded()
local result = first == 41 and second == 42 and environment.answer == 42
    and getfenv(read) == environment and getfenv(loaded) == environment
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_STRING_LIBRARY_SOURCE: &str = r#"
local values = { "a", "c" }
table.insert(values, 2, "b")
local removed = table.remove(values, 3)
local joined = table.concat(values, "-")
local first, second = string.byte("AZ", 1, 2)
local packed = table.pack(7, nil, 9)
local packed_first, packed_second, packed_third = table.unpack(packed, 1, 3)
return string.len(joined) + first + second
    + (string.reverse(joined) == "b-a" and removed == "c"
        and string.char(65, 0, 255) == "A\0\255"
        and string.rep("ab", 3, "-") == "ababab"
        and string.lower("A\255Z") == "a\255z"
        and string.upper("a\255z") == "A\255Z"
        and packed.n == 3 and packed_first == 7
        and packed_second == nil and packed_third == 9 and 1 or 0)
"#;
const TABLE_STRING_LIBRARY_REFERENCE_SOURCE: &str = r#"
local values = { "a", "c" }
table.insert(values, 2, "b")
local removed = table.remove(values, 3)
local joined = table.concat(values, "-")
local first, second = string.byte("AZ", 1, 2)
local packed = table.pack(7, nil, 9)
local packed_first, packed_second, packed_third = table.unpack(packed, 1, 3)
local result = string.len(joined) + first + second
    + (string.reverse(joined) == "b-a" and removed == "c"
        and string.char(65, 0, 255) == "A\0\255"
        and string.rep("ab", 3, "-") == "ababab"
        and string.lower("A\255Z") == "a\255z"
        and string.upper("a\255z") == "A\255Z"
        and packed.n == 3 and packed_first == 7
        and packed_second == nil and packed_third == 9 and 1 or 0)
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_LIBRARY_SOURCE: &str = r#"
return math.abs(-3) + math.floor(2.9) + math.ceil(2.1) + math.sqrt(16)
    + math.min(8, 1, 5) + math.max(2, 9, 4)
    + math.floor(math.exp(1)) + math.floor(math.log(8, 2))
    + math.sin(0) + math.cos(0) + math.tan(0)
    + math.floor(math.deg(math.rad(90)))
    + (math.pi > 3 and 1 or 0) + (math.huge > 1e300 and 1 or 0)
"#;
const MATH_LIBRARY_REFERENCE_SOURCE: &str = r#"
local result = math.abs(-3) + math.floor(2.9) + math.ceil(2.1) + math.sqrt(16)
    + math.min(8, 1, 5) + math.max(2, 9, 4)
    + math.floor(math.exp(1)) + math.floor(math.log(8, 2))
    + math.sin(0) + math.cos(0) + math.tan(0)
    + math.floor(math.deg(math.rad(90)))
    + (math.pi > 3 and 1 or 0) + (math.huge > 1e300 and 1 or 0)
print(type(result) .. ":" .. tostring(result))
"#;
const GENERIC_FOR_COROUTINE_SOURCE: &str = r#"
local calls = 0
local function iterator(state, control)
    calls += 1
    if control >= 2 then
        return nil
    end
    local resumed = coroutine.yield("step" .. calls)
    return control + 1, state + resumed
end
local thread = coroutine.create(function()
    local total = 0
    for key, value in iterator, 40, 0 do
        total += key + value
    end
    return calls, total
end)
local first_ok, first = coroutine.resume(thread)
local second_ok, second = coroutine.resume(thread, 1)
local third_ok, count, total = coroutine.resume(thread, 2)
return first_ok and first == "step1"
    and second_ok and second == "step2"
    and third_ok and count == 3 and total == 86
    and coroutine.status(thread) == "dead"
"#;
const GENERIC_FOR_COROUTINE_REFERENCE_SOURCE: &str = r#"
local calls = 0
local function iterator(state, control)
    calls += 1
    if control >= 2 then
        return nil
    end
    local resumed = coroutine.yield("step" .. calls)
    return control + 1, state + resumed
end
local thread = coroutine.create(function()
    local total = 0
    for key, value in iterator, 40, 0 do
        total += key + value
    end
    return calls, total
end)
local first_ok, first = coroutine.resume(thread)
local second_ok, second = coroutine.resume(thread, 1)
local third_ok, count, total = coroutine.resume(thread, 2)
local result = first_ok and first == "step1"
    and second_ok and second == "step2"
    and third_ok and count == 3 and total == 86
    and coroutine.status(thread) == "dead"
print(type(result) .. ":" .. tostring(result))
"#;
const ERROR_HANDLER_CALL_SOURCE: &str = r#"
local ok, value = xpcall(function(input)
    return input * 2
end, function(message)
    return "unexpected"
end, 5)
local failed, message = xpcall(function()
    error("boom")
end, function(caught)
    return "handled:" .. type(caught)
end)
return ok and value == 10 and not failed and message == "handled:string"
"#;
const ERROR_HANDLER_CALL_REFERENCE_SOURCE: &str = r#"
local ok, value = xpcall(function(input)
    return input * 2
end, function(message)
    return "unexpected"
end, 5)
local failed, message = xpcall(function()
    error("boom")
end, function(caught)
    return "handled:" .. type(caught)
end)
local result = ok and value == 10 and not failed and message == "handled:string"
print(type(result) .. ":" .. tostring(result))
"#;
const NUMBER_CONVERSION_SOURCE: &str = r#"
return tonumber("12.5") + tonumber("ff", 16) + tonumber(3)
    + (typeof(tonumber("invalid")) == "nil" and 1 or 0)
"#;
const NUMBER_CONVERSION_REFERENCE_SOURCE: &str = r#"
local result = tonumber("12.5") + tonumber("ff", 16) + tonumber(3)
    + (typeof(tonumber("invalid")) == "nil" and 1 or 0)
print(type(result) .. ":" .. tostring(result))
"#;
const COROUTINE_SOURCE: &str = r##"
local wrapped = coroutine.wrap(function(first)
    local resumed = coroutine.yield(first + 1)
    return resumed + 1
end)
local wrapped_first = wrapped(3)
local wrapped_second = wrapped(7)
local disposable = coroutine.create(function() end)
local closed = coroutine.close(disposable)

local thread = coroutine.create(function()
    local ok, value = pcall(function()
        local function nested()
            return coroutine.yield("pause")
        end
        return nested() + 1
    end)
    return ok, value
end)
local first_ok, paused = coroutine.resume(thread)
local suspended = coroutine.status(thread)
local second_ok, protected_ok, result = coroutine.resume(thread, 41)
local failing = coroutine.create(function()
    local ok, message = pcall(function()
        coroutine.yield("error pause")
        error("resume boom")
    end)
    return ok, type(message)
end)
local failing_first, failing_pause = coroutine.resume(failing)
local failing_second, failing_ok, failing_type = coroutine.resume(failing)
local handled = coroutine.create(function()
    return xpcall(function()
        coroutine.yield("handled pause")
        error("handled boom")
    end, function()
        return "handled"
    end)
end)
local handled_first, handled_pause = coroutine.resume(handled)
local handled_second, handled_ok, handled_message = coroutine.resume(handled)
return first_ok and paused == "pause" and suspended == "suspended"
    and second_ok and protected_ok and result == 42
    and coroutine.status(thread) == "dead"
    and wrapped_first == 4 and wrapped_second == 8
    and closed and coroutine.status(disposable) == "dead"
    and select("#", coroutine.running()) == 1
    and coroutine.isyieldable()
    and failing_first and failing_pause == "error pause"
    and failing_second and not failing_ok and failing_type == "string"
    and handled_first and handled_pause == "handled pause"
    and handled_second and not handled_ok and handled_message == "handled"
"##;
const COROUTINE_REFERENCE_SOURCE: &str = r##"
local wrapped = coroutine.wrap(function(first)
    local resumed = coroutine.yield(first + 1)
    return resumed + 1
end)
local wrapped_first = wrapped(3)
local wrapped_second = wrapped(7)
local disposable = coroutine.create(function() end)
local closed = coroutine.close(disposable)

local thread = coroutine.create(function()
    local ok, value = pcall(function()
        local function nested()
            return coroutine.yield("pause")
        end
        return nested() + 1
    end)
    return ok, value
end)
local first_ok, paused = coroutine.resume(thread)
local suspended = coroutine.status(thread)
local second_ok, protected_ok, result = coroutine.resume(thread, 41)
local failing = coroutine.create(function()
    local ok, message = pcall(function()
        coroutine.yield("error pause")
        error("resume boom")
    end)
    return ok, type(message)
end)
local failing_first, failing_pause = coroutine.resume(failing)
local failing_second, failing_ok, failing_type = coroutine.resume(failing)
local handled = coroutine.create(function()
    return xpcall(function()
        coroutine.yield("handled pause")
        error("handled boom")
    end, function()
        return "handled"
    end)
end)
local handled_first, handled_pause = coroutine.resume(handled)
local handled_second, handled_ok, handled_message = coroutine.resume(handled)
local value = first_ok and paused == "pause" and suspended == "suspended"
    and second_ok and protected_ok and result == 42
    and coroutine.status(thread) == "dead"
    and wrapped_first == 4 and wrapped_second == 8
    and closed and coroutine.status(disposable) == "dead"
    and select("#", coroutine.running()) == 1
    and coroutine.isyieldable()
    and failing_first and failing_pause == "error pause"
    and failing_second and not failing_ok and failing_type == "string"
    and handled_first and handled_pause == "handled pause"
    and handled_second and not handled_ok and handled_message == "handled"
print(type(value) .. ":" .. tostring(value))
"##;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("blu-conformance: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(env::args_os().skip(1))?;
    verify_checkout(&args.source)?;
    verify_executable(&args.upstream)?;
    let lua_references = verify_lua_references(&args.lua_source)?;
    let compiler = args
        .upstream
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(executable_name("luau-compile"));
    verify_executable(&compiler)?;

    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (scalar_count, bytecode_version) =
        verify_scalar_cases(&compiler, &args.upstream, temporary.path())?;
    verify_program_case(
        "table identity and split storage",
        TABLE_SOURCE,
        TABLE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "yielding generic-for iterator",
        GENERIC_FOR_COROUTINE_SOURCE,
        GENERIC_FOR_COROUTINE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "numeric for loop",
        LOOP_SOURCE,
        LOOP_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "captureless function call",
        FUNCTION_SOURCE,
        FUNCTION_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "mutable captured local",
        CAPTURE_SOURCE,
        CAPTURE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "nested upvalue capture",
        NESTED_CAPTURE_SOURCE,
        NESTED_CAPTURE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "child mutation visible in parent",
        PARENT_CAPTURE_SOURCE,
        PARENT_CAPTURE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "variadic arguments",
        VARARGS_SOURCE,
        VARARGS_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "multiple return forwarding",
        MULTRET_SOURCE,
        MULTRET_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "constant table template",
        TABLE_LITERAL_SOURCE,
        TABLE_LITERAL_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "generic for iteration",
        GENERIC_FOR_SOURCE,
        GENERIC_FOR_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "string method call",
        METHOD_CALL_SOURCE,
        METHOD_CALL_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "direct table iteration",
        DIRECT_ITERATION_SOURCE,
        DIRECT_ITERATION_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "base type and tostring",
        BASE_LIBRARY_SOURCE,
        BASE_LIBRARY_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "table metatable index inheritance",
        METATABLE_SOURCE,
        METATABLE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "callable index metamethod",
        CALLABLE_INDEX_SOURCE,
        CALLABLE_INDEX_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "table and callable newindex metamethods",
        NEWINDEX_SOURCE,
        NEWINDEX_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "arithmetic metamethods",
        ARITHMETIC_METAMETHOD_SOURCE,
        ARITHMETIC_METAMETHOD_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "unary and length metamethods",
        UNARY_METAMETHOD_SOURCE,
        UNARY_METAMETHOD_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "comparison metamethods",
        COMPARISON_METAMETHOD_SOURCE,
        COMPARISON_METAMETHOD_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "call and concatenation metamethods",
        CALL_AND_CONCAT_METAMETHOD_SOURCE,
        CALL_AND_CONCAT_METAMETHOD_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "raw base operations",
        RAW_BASE_SOURCE,
        RAW_BASE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "assert and select",
        ASSERT_SELECT_SOURCE,
        ASSERT_SELECT_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "protected calls",
        PROTECTED_CALL_SOURCE,
        PROTECTED_CALL_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "table and string libraries",
        TABLE_STRING_LIBRARY_SOURCE,
        TABLE_STRING_LIBRARY_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "math library",
        MATH_LIBRARY_SOURCE,
        MATH_LIBRARY_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "error-handler protected calls",
        ERROR_HANDLER_CALL_SOURCE,
        ERROR_HANDLER_CALL_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "number conversion",
        NUMBER_CONVERSION_SOURCE,
        NUMBER_CONVERSION_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_program_case(
        "coroutine yield and protected resume",
        COROUTINE_SOURCE,
        COROUTINE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "owned synchronous library callbacks",
        OWNED_CALLBACK_SOURCE,
        OWNED_CALLBACK_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case(
            &format!("owned synchronous library callbacks ({name})"),
            OWNED_CALLBACK_SOURCE,
            OWNED_CALLBACK_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("package.preload require ({name})"),
            PACKAGE_PRELOAD_SOURCE,
            PACKAGE_PRELOAD_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        if profile == SemanticProfile::Lua51 {
            verify_owned_environment_case(
                &format!("Lua 5.1 function environments ({name})"),
                LUA51_ENVIRONMENT_SOURCE,
                LUA51_ENVIRONMENT_REFERENCE_SOURCE,
                profile,
                executable,
                temporary.path(),
            )?;
        }
        verify_owned_load_reader_case(
            &format!("reader-function load ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case(
            &format!("to-be-closed error argument ({name})"),
            CLOSE_ERROR_SOURCE,
            CLOSE_ERROR_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("to-be-closed reverse error unwind ({name})"),
            CLOSE_REVERSE_SOURCE,
            CLOSE_REVERSE_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("yielding to-be-closed handler ({name})"),
            CLOSE_YIELD_SOURCE,
            CLOSE_YIELD_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case(
            &format!("lexical _ENV override ({name})"),
            ENVIRONMENT_SOURCE,
            ENVIRONMENT_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("default _ENV closure ({name})"),
            DEFAULT_ENVIRONMENT_SOURCE,
            DEFAULT_ENVIRONMENT_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_load_case(
            &format!("environment-aware load ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("package cache tables ({name})"),
            PACKAGE_SOURCE,
            PACKAGE_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
    }

    let portable_source = temporary.path().join("portable.lua");
    fs::write(&portable_source, PORTABLE_SOURCE).map_err(|error| error.to_string())?;
    let portable_bytecode = Command::new(&compiler)
        .arg("--binary")
        .arg(&portable_source)
        .output()
        .map_err(|error| format!("failed to execute {}: {error}", compiler.display()))?;
    ensure_success(&compiler, &portable_bytecode)?;
    let portable_chunk = load(&portable_bytecode.stdout, LoadLimits::default())
        .map_err(|error| format!("Blu rejected portable upstream bytecode: {error}"))?;
    let mut blu = Vm::default();
    let results = blu.execute(&portable_chunk).map_err(|error| {
        format!(
            "Blu failed portable upstream bytecode: {error}\n{}",
            disassemble(&portable_chunk)
        )
    })?;
    if !results.is_empty() {
        return Err(format!(
            "Blu portable program returned unexpected values {results:?}"
        ));
    }
    let output = blu.take_output();
    if output != b"14\nlu\n" {
        return Err(format!(
            "Blu portable program returned unexpected output {:?}",
            String::from_utf8_lossy(&output)
        ));
    }
    let bundled_chunk = SourceCompiler::default()
        .compile(PORTABLE_SOURCE)
        .map_err(|error| format!("bundled source compiler failed portable source: {error}"))?;
    let mut bundled_vm = Vm::default();
    bundled_vm.execute(&bundled_chunk).map_err(|error| {
        format!(
            "Blu failed bytecode from bundled source compiler: {error}\n{}",
            disassemble(&bundled_chunk)
        )
    })?;
    if bundled_vm.take_output() != b"14\nlu\n" {
        return Err("bundled source compiler produced incorrect portable output".into());
    }
    verify_portable_reference("Luau", &args.upstream, &portable_source)?;
    for (name, executable) in &lua_references {
        verify_portable_reference(name, executable, &portable_source)?;
    }

    println!("pinned Luau revision: {PINNED_REVISION}");
    println!("bytecode version: {bytecode_version}");
    println!("scalar differential corpus: pass ({scalar_count} cases)");
    println!(
        "program differential corpus: pass (tables, loops, iteration, methods, metamethods, closures, captures, varargs, multret, coroutines, default/lexical environments, environment-aware load, package.preload require, to-be-closed error/reverse/yield paths)"
    );
    println!("owned callback differential corpus: pass (Luau, Lua 5.1-5.5 profiles)");
    println!("portable reference matrix: pass (Luau, Lua 5.1-5.5)");
    Ok(())
}

fn verify_program_case(
    name: &str,
    source: &str,
    reference_source: &str,
    compiler: &Path,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_path = temporary.join("program.luau");
    fs::write(&source_path, source).map_err(|error| error.to_string())?;
    let bytecode = Command::new(compiler)
        .arg("--binary")
        .arg(&source_path)
        .output()
        .map_err(|error| format!("failed to execute {}: {error}", compiler.display()))?;
    ensure_success(compiler, &bytecode)?;
    let chunk = load(&bytecode.stdout, LoadLimits::default())
        .map_err(|error| format!("Blu rejected program case {name:?}: {error}"))?;
    let result = Vm::new(Dialect::Luau).execute(&chunk).map_err(|error| {
        format!(
            "Blu failed program case {name:?}: {error}\n{}",
            disassemble(&chunk)
        )
    })?;
    if result.len() != 1 {
        return Err(format!(
            "Blu returned {} values for program case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let reference_path = temporary.join("program-reference.luau");
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute reference {}: {error}",
                upstream.display()
            )
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "program case {name:?} differs: Blu={result:?}, Luau={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_program_case(
    name: &str,
    source: &str,
    reference_source: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-conformance.blu",
        source.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned source case {name:?} was invalid: {error}"))?;
    let identity = CompilerIdentity::new(
        CompilerId::new(*b"blu-owned-v1\0\0\0\0"),
        "blu-owned",
        env!("CARGO_PKG_VERSION"),
        None,
        IdentityLimits::default(),
    )
    .map_err(|error| format!("owned compiler identity failed for {name:?}: {error}"))?;
    let compilation = OwnedCompiler::default()
        .compile(&source_file, profile, identity)
        .map_err(|error| format!("owned compiler failed case {name:?}: {error}"))?;
    let result = Vm::default()
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned runtime returned {} values for case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let reference_path = temporary.join("owned-program-reference.luau");
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| format!("failed to execute owned reference {upstream:?}: {error}"))?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned program case {name:?} differs: Blu={result:?}, Luau={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_load_case(
    name: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(LOAD_ENVIRONMENT_SOURCE, profile)
        .map_err(|error| format!("owned load failed for case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned load returned {} values for case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let reference_path = temporary.join("owned-load-reference.luau");
    fs::write(&reference_path, LOAD_ENVIRONMENT_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| format!("failed to execute owned load reference {upstream:?}: {error}"))?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned load case {name:?} differs: Blu={result:?}, Lua={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_load_reader_case(
    name: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(LOAD_READER_SOURCE, profile)
        .map_err(|error| format!("owned reader load failed for case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned reader load returned {} values for case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let reference_path = temporary.join("owned-load-reader-reference.luau");
    fs::write(&reference_path, LOAD_READER_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute owned reader load reference {upstream:?}: {error}")
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned reader load case {name:?} differs: Blu={result:?}, Lua={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_environment_case(
    name: &str,
    source: &str,
    reference_source: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(source, profile)
        .map_err(|error| format!("owned environment case failed for {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned environment case returned {} values for {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let reference_path = temporary.join("owned-environment-reference.luau");
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute owned environment reference {upstream:?}: {error}")
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned environment case {name:?} differs: Blu={result:?}, Lua={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_scalar_cases(
    compiler: &Path,
    upstream: &Path,
    temporary: &Path,
) -> Result<(usize, u8), String> {
    let mut bytecode_version = None;
    for (index, (name, expression)) in SCALAR_CASES.iter().enumerate() {
        let return_source = temporary.join(format!("scalar-{index}.luau"));
        fs::write(&return_source, format!("return {expression}\n"))
            .map_err(|error| error.to_string())?;
        let bytecode = Command::new(compiler)
            .arg("--binary")
            .arg(&return_source)
            .output()
            .map_err(|error| format!("failed to execute {}: {error}", compiler.display()))?;
        ensure_success(compiler, &bytecode)?;
        let chunk = load(&bytecode.stdout, LoadLimits::default())
            .map_err(|error| format!("Blu rejected scalar case {name:?}: {error}"))?;
        bytecode_version.get_or_insert(chunk.version);
        let blu_result = Vm::new(Dialect::Luau)
            .execute(&chunk)
            .map_err(|error| format!("Blu failed scalar case {name:?}: {error}"))?;
        if blu_result.len() != 1 {
            return Err(format!(
                "Blu returned {} values for scalar case {name:?}, expected one",
                blu_result.len()
            ));
        }
        let blu_result = canonical_value(&blu_result[0])?;

        let print_source = temporary.join(format!("scalar-reference-{index}.luau"));
        fs::write(
            &print_source,
            format!("local value = {expression}\nprint(type(value) .. \":\" .. tostring(value))\n"),
        )
        .map_err(|error| error.to_string())?;
        let reference = Command::new(upstream)
            .arg(&print_source)
            .output()
            .map_err(|error| {
                format!(
                    "failed to execute reference {}: {error}",
                    upstream.display()
                )
            })?;
        ensure_success(upstream, &reference)?;
        let reference = String::from_utf8_lossy(&reference.stdout);
        if blu_result != reference.trim() {
            return Err(format!(
                "scalar case {name:?} differs: Blu={blu_result:?}, Luau={:?}",
                reference.trim()
            ));
        }
    }
    Ok((
        SCALAR_CASES.len(),
        bytecode_version.ok_or("scalar corpus is empty")?,
    ))
}

fn canonical_value(value: &Value) -> Result<String, String> {
    match value {
        Value::Nil => Ok("nil:nil".into()),
        Value::Boolean(value) => Ok(format!("boolean:{value}")),
        Value::Number(value) => Ok(format!("number:{value}")),
        Value::Integer(value) => Ok(format!("number:{value}")),
        Value::String(value) => std::str::from_utf8(value)
            .map(|value| format!("string:{value}"))
            .map_err(|error| format!("Blu returned a non-UTF-8 scalar string: {error}")),
        _ => Err(format!(
            "Blu returned an unsupported differential value {value:?}"
        )),
    }
}

fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

fn verify_checkout(path: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    ensure_success(Path::new("git"), &output)?;
    let actual = String::from_utf8_lossy(&output.stdout);
    if actual.trim() != PINNED_REVISION {
        return Err(format!(
            "upstream checkout is {}, expected {PINNED_REVISION}",
            actual.trim()
        ));
    }
    Ok(())
}

fn verify_executable(path: &Path) -> Result<(), String> {
    let output = Command::new(path)
        .arg("--help")
        .output()
        .map_err(|error| format!("failed to execute {}: {error}", path.display()))?;
    ensure_success(path, &output)
}

fn verify_lua_references(source: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    LUA_REFERENCES
        .iter()
        .map(|(version, expected_name)| {
            let executable = source
                .join(format!("lua-{version}"))
                .join("src")
                .join(executable_name("lua"));
            let output = Command::new(&executable)
                .args(["-e", "print(_VERSION)"])
                .output()
                .map_err(|error| format!("failed to execute {}: {error}", executable.display()))?;
            ensure_success(&executable, &output)?;
            let actual_name = String::from_utf8_lossy(&output.stdout);
            if actual_name.trim() != *expected_name {
                return Err(format!(
                    "{} identifies as {:?}, expected {expected_name:?}",
                    executable.display(),
                    actual_name.trim()
                ));
            }
            Ok((expected_name.to_string(), executable))
        })
        .collect()
}

fn verify_portable_reference(name: &str, executable: &Path, source: &Path) -> Result<(), String> {
    let output = Command::new(executable)
        .arg(source)
        .output()
        .map_err(|error| format!("failed to execute {name} reference: {error}"))?;
    ensure_success(executable, &output)?;
    let actual = String::from_utf8_lossy(&output.stdout);
    if actual.trim() != PORTABLE_EXPECTED {
        return Err(format!(
            "{name} returned {:?} for the portable reference, expected {PORTABLE_EXPECTED:?}",
            actual.trim()
        ));
    }
    Ok(())
}

fn ensure_success(path: &Path, output: &std::process::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed with {}: {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Args {
    upstream: PathBuf,
    source: PathBuf,
    lua_source: PathBuf,
}

impl Args {
    fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut upstream = None;
        let mut source = None;
        let mut lua_source = None;
        let mut args = args;
        while let Some(argument) = args.next() {
            match argument.to_str() {
                Some("--upstream") => {
                    upstream = Some(args.next().ok_or("--upstream requires a path")?.into());
                }
                Some("--source") => {
                    source = Some(args.next().ok_or("--source requires a path")?.into());
                }
                Some("--lua-source") => {
                    lua_source = Some(args.next().ok_or("--lua-source requires a path")?.into());
                }
                _ => {
                    return Err(format!(
                        "usage: blu-conformance --upstream <luau> --source <luau-checkout> \
                         --lua-source <lua-checkouts>; \
                         unexpected argument {}",
                        argument.to_string_lossy()
                    ));
                }
            }
        }
        Ok(Self {
            upstream: upstream.ok_or("missing --upstream")?,
            source: source.ok_or("missing --source")?,
            lua_source: lua_source.ok_or("missing --lua-source")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_reference_runtime_and_source() {
        let args = Args::parse(
            [
                "--upstream",
                "/tmp/build/luau",
                "--source",
                "/tmp/source",
                "--lua-source",
                "/tmp/lua",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        assert_eq!(
            args,
            Args {
                upstream: "/tmp/build/luau".into(),
                source: "/tmp/source".into(),
                lua_source: "/tmp/lua".into(),
            }
        );
        assert!(Args::parse(std::iter::empty()).is_err());
    }
}
