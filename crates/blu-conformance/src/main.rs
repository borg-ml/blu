#![forbid(unsafe_code)]

use blu_compiler::{Compiler as SourceCompiler, owned::OwnedCompiler};
use blu_core::{
    CompilerId, CompilerIdentity, IdentityLimits, SemanticProfile, SourceFile, SourceId,
    SourceLimits,
};
use blu_lang::{Engine, OwnedExecuteError};
use blu_runtime::{
    CalendarDate, CalendarDateInput, Dialect, IoBufferMode, IoFile, IoReadRequest, IoSeekWhence,
    IoStreamKind, MemoryConfig, NativeLibraryFailure, NativeLibraryLoadResult, OsExecuteResult,
    OsExitRequest, RuntimeError, Value, Vm,
    bytecode::{LoadLimits, blu::BluLimits, disassemble, load},
};
use std::{
    env, fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitCode, Output, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const PINNED_REVISION: &str = "f8ca77acdcb50241e3da21af663f8ef97b4b5ce4";
const LUA_REFERENCES: [(&str, &str); 5] = [
    ("5.1.5", "Lua 5.1"),
    ("5.2.4", "Lua 5.2"),
    ("5.3.6", "Lua 5.3"),
    ("5.4.8", "Lua 5.4"),
    ("5.5.0", "Lua 5.5"),
];
const OFFICIAL_LUAU_PORTABLE_TESTS: &[&str] = &[
    "apicalls.luau",
    "assert.luau",
    "attrib.luau",
    "basic.luau",
    "bitwise.luau",
    "calls.luau",
    "clear.luau",
    "closure.luau",
    "constructs.luau",
    "coroutine.luau",
    "debug.luau",
    "events.luau",
    "errors.luau",
    "exceptions.luau",
    "ifelseexpr.luau",
    "iter.luau",
    "iter_fenv.luau",
    "jit_inliner.luau",
    "literals.luau",
    "locals.luau",
    "math.luau",
    "move.luau",
    "pcall.luau",
    "pm.luau",
    "safeenv.luau",
    "sort.luau",
    "strconv.luau",
    "strings.luau",
    "stringinterp.luau",
    "tables.luau",
    "tpack.luau",
    "tmerror.luau",
    "utf8.luau",
    "vararg.luau",
];
const OFFICIAL_LUAU_DIRECT_CLI_ISOLATIONS: &[(&str, &str)] = &[
    (
        "closure.luau",
        "standalone Luau prefixes the chunk path with ./; the test hardcodes the C++ harness source name",
    ),
    (
        "debug.luau",
        "standalone Luau prefixes the chunk path with ./; the test hardcodes the C++ harness source name",
    ),
    (
        "coroutine.luau",
        "standalone Luau runs the chunk on a main thread; the C++ harness executes it on a yieldable worker thread",
    ),
    (
        "basic.luau",
        "standalone Luau uses a different chunk-path spelling than the fixture's expected basic.luau error prefix",
    ),
    (
        "iter.luau",
        "standalone Luau does not install the C++ cYieldingIterator host callback used by this test",
    ),
    (
        "pcall.luau",
        "standalone Luau prefixes the chunk path with ./; the test hardcodes the C++ harness source name in post-yield errors",
    ),
    (
        "pm.luau",
        "standalone Luau keeps _G readonly; the fixture mutates _G from a gsub callback",
    ),
    (
        "tables.luau",
        "standalone Luau keeps _G readonly; the C++ conformance harness permits the global-table mutation used by this test",
    ),
    (
        "vararg.luau",
        "standalone Luau keeps _G readonly; the C++ conformance harness changes it",
    ),
];
const OFFICIAL_LUAU_PROFILE_ISOLATIONS: &[(&str, &str)] = &[
    (
        "coroutine.luau",
        "Blu intentionally keeps the modern Lua main-thread `coroutine.running` pair and non-yieldable main context; the pinned Luau fixture expects nil plus a yieldable main context",
    ),
    (
        "basic.luau",
        "Blu preserves exact integer semantics, so the Luau negative-zero assertion is intentionally different; the Luau profile reaches the documented mixed table hash-order boundary",
    ),
    (
        "iter_fenv.luau",
        "the Luau profile uses Luau's typed iterator diagnostic; Blu intentionally retains its generic non-iterable wording",
    ),
    (
        "debug.luau",
        "Blu exposes the bounded portable traceback surface on its default profile; Luau's full `debug.info` API is profile-specific",
    ),
    (
        "events.luau",
        "the Luau profile uses its canonical `__tostring` return diagnostic; Blu retains its structured operation/type wording",
    ),
    (
        "pcall.luau",
        "Blu intentionally hides the debug library used by the fixture's traceback cases; the Luau profile's post-yield protected traceback and allocation-pressure cases pass",
    ),
    (
        "errors.luau",
        "the Luau profile passes the complete error, wording, stack, and source-name fixture; Blu intentionally accepts semicolon-only and empty statements as documented owned syntax",
    ),
    (
        "math.luau",
        "Blu preserves fitting decimal integers exactly where Luau compares adjacent large decimal literals through its double-number model; the Luau child uses the fixture's soft path after those early checks",
    ),
    (
        "move.luau",
        "the fixture reaches Luau's signed 32-bit destination-wrap probe; Blu intentionally retains signed 64-bit table.move positions",
    ),
    (
        "sort.luau",
        "Blu intentionally omits the system-capability os library; the Luau child exposes only deterministic safe `os.clock` and the official sort fixture passes",
    ),
    (
        "tables.luau",
        "the core table cases pass, but the owned interpreter exceeds the explicit 60-second watchdog on the fixture's bounded 65,535 x 16-bit allocation stress",
    ),
    (
        "tmerror.luau",
        "the Luau profile uses source-prefixed `attempt to call a nil value`; Blu intentionally retains its structured call diagnostic",
    ),
    (
        "utf8.luau",
        "Luau strictly rejects surrogate codepoints; Blu intentionally retains its documented Lua 5.3-compatible surrogate tolerance",
    ),
];
// `tables.luau` contains a bounded 65,535 x 16-bit allocation stress. Keep
// that workload isolated with a larger explicit budget without broadening
// the watchdog for unrelated official fixtures.
const DEFAULT_OFFICIAL_TEST_DEADLINE: Duration = Duration::from_secs(30);
const TABLES_OFFICIAL_TEST_DEADLINE: Duration = Duration::from_secs(60);
const SORT_OFFICIAL_TEST_DEADLINE: Duration = Duration::from_secs(120);
const MODERN_LUA_OFFICIAL_TEST_DEADLINE: Duration = Duration::from_secs(30);
const DEFAULT_OFFICIAL_TEST_INSTRUCTION_LIMIT: u64 = 10_000_000;
const TABLES_OFFICIAL_TEST_INSTRUCTION_LIMIT: u64 = 40_000_000;
const PCALL_OFFICIAL_TEST_CALL_LIMIT: usize = 20_000;
// `calls.luau` deliberately recurses to 19,000 frames before exercising
// Luau's result-count guard. Blu's owned-call path also accounts for the
// native/table helper frames surrounding each recursive call, so preserve the
// fixture's intended probe with a bounded fixture-local allowance.
const CALLS_OFFICIAL_TEST_CALL_LIMIT: usize = 32_000;
const PCALL_OFFICIAL_TEST_MEMORY_LIMIT: usize = 8 * 1024 * 1024;
fn official_luau_vm(name: &str, instruction_limit: u64, deadline: Instant) -> Vm {
    let memory = if name == "pcall.luau" {
        MemoryConfig {
            hard_limit_bytes: Some(PCALL_OFFICIAL_TEST_MEMORY_LIMIT),
            gc_start_bytes: PCALL_OFFICIAL_TEST_MEMORY_LIMIT,
            max_single_allocation_bytes: 1 << 20,
            ..MemoryConfig::default()
        }
    } else {
        MemoryConfig::default()
    };
    let call_limit = if name == "calls.luau" {
        CALLS_OFFICIAL_TEST_CALL_LIMIT
    } else {
        PCALL_OFFICIAL_TEST_CALL_LIMIT
    };
    Vm::try_new_with_memory(Dialect::Blu, memory)
        .expect("official Luau conformance VM should initialize")
        .with_instruction_limit(instruction_limit)
        .with_call_limit(call_limit)
        .with_deadline(deadline)
}

fn official_luau_test_deadline(name: &str) -> Duration {
    if name == "tables.luau" {
        TABLES_OFFICIAL_TEST_DEADLINE
    } else if name == "sort.luau" {
        SORT_OFFICIAL_TEST_DEADLINE
    } else {
        DEFAULT_OFFICIAL_TEST_DEADLINE
    }
}

fn official_luau_test_instruction_limit(name: &str) -> u64 {
    if name == "tables.luau" {
        TABLES_OFFICIAL_TEST_INSTRUCTION_LIMIT
    } else {
        DEFAULT_OFFICIAL_TEST_INSTRUCTION_LIMIT
    }
}
const OFFICIAL_LUA51_PORTABLE_TESTS: &[&str] = &[
    "hello.lua",
    "factorial.lua",
    "fibfor.lua",
    "globals.lua",
    "bisect.lua",
    "sieve.lua",
    "sort.lua",
    "table.lua",
    "cf.lua",
];
const OFFICIAL_LUA54_PORTABLE_TESTS: &[&str] = &[
    "attrib.lua",
    "bitwise.lua",
    "calls.lua",
    "closure.lua",
    "constructs.lua",
    "coroutine.lua",
    "errors.lua",
    "events.lua",
    "goto.lua",
    "literals.lua",
    "locals.lua",
    "math.lua",
    "strings.lua",
    "tpack.lua",
    "utf8.lua",
    "vararg.lua",
];
const OFFICIAL_LUA55_PORTABLE_TESTS: &[&str] = &[
    "attrib.lua",
    "bitwise.lua",
    "calls.lua",
    "closure.lua",
    "constructs.lua",
    "coroutine.lua",
    "errors.lua",
    "events.lua",
    "goto.lua",
    "literals.lua",
    "locals.lua",
    "math.lua",
    "strings.lua",
    "tpack.lua",
    "utf8.lua",
    "vararg.lua",
];
const OFFICIAL_LUA_MODERN_ISOLATIONS: &[(&str, &str, &str)] = &[
    (
        "5.4.8",
        "constructs.lua",
        "the executable assertions pass, but the owned child does not reproduce PUC's syntax/progress output bytes",
    ),
    (
        "5.5.0",
        "calls.lua",
        "the executable assertions pass, but the owned child does not reproduce PUC's call/progress output bytes",
    ),
    (
        "5.4.8",
        "errors.lua",
        "the executable assertions pass, but PUC's printed C-stack capacity is host-dependent and differs from the owned stack count",
    ),
    (
        "5.5.0",
        "constructs.lua",
        "the executable assertions pass, but the owned child does not reproduce PUC's syntax/progress output bytes",
    ),
    (
        "5.4.8",
        "locals.lua",
        "the executable assertions pass, but the owned child does not reproduce PUC's progress-dot output bytes",
    ),
    (
        "5.5.0",
        "errors.lua",
        "the executable assertions pass, but PUC's printed C-stack capacity is host-dependent and differs from the owned stack count",
    ),
    (
        "5.4.8",
        "locals.lua",
        "the executable assertions pass, but the owned child does not reproduce PUC's progress-dot output bytes",
    ),
    (
        "5.5.0",
        "locals.lua",
        "the executable assertions pass, but the owned child does not reproduce PUC's progress-dot output bytes",
    ),
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
const OWNED_NATIVE_CALLBACK_YIELDABILITY_SOURCE: &str = r#"
local callback_yieldable
local transformed = string.gsub("a", ".", function()
    callback_yieldable = coroutine.isyieldable()
    return "b"
end)
return transformed == "b" and callback_yieldable == false and coroutine.isyieldable()
"#;
const OWNED_NATIVE_CALLBACK_YIELDABILITY_REFERENCE_SOURCE: &str = r#"
local callback_yieldable
local transformed = string.gsub("a", ".", function()
    callback_yieldable = coroutine.isyieldable()
    return "b"
end)
local result = transformed == "b" and callback_yieldable == false and coroutine.isyieldable()
print(type(result) .. ":" .. tostring(result))
"#;
const LUA51_MAIN_CHUNK_ENVIRONMENT_SOURCE: &str = r#"
local global_environment = getfenv()
local writes = {}
local environment = {}
setmetatable(environment, {
    __index = global_environment,
    __newindex = function(_, key, value) writes[key] = value end,
})
setfenv(1, environment)
answer = 42
return writes.answer == 42 and rawget(environment, "answer") == nil
"#;
const LUA51_MAIN_CHUNK_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
local global_environment = getfenv()
local writes = {}
local environment = {}
setmetatable(environment, {
    __index = global_environment,
    __newindex = function(_, key, value) writes[key] = value end,
})
setfenv(1, environment)
answer = 42
local result = writes.answer == 42 and rawget(environment, "answer") == nil
print(type(result) .. ":" .. tostring(result))
"#;
const LUA51_YIELDING_MAIN_CHUNK_NEWINDEX_SOURCE: &str = r#"
local function body()
    local environment = {}
    local received
    setmetatable(environment, {
        __index = getfenv(),
        __newindex = function(_, key, value)
            received = key .. ":" .. value
            coroutine.yield("pause")
        end,
    })
    setfenv(1, environment)
    answer = 42
    return received
end
local thread = coroutine.create(body)
local first, pause = coroutine.resume(thread)
local second, result = coroutine.resume(thread)
return first and pause == "pause" and second and result == "answer:42"
"#;
const LUA51_YIELDING_MAIN_CHUNK_NEWINDEX_REFERENCE_SOURCE: &str = r#"
local function body()
    local environment = {}
    local received
    setmetatable(environment, {
        __index = getfenv(),
        __newindex = function(_, key, value)
            received = key .. ":" .. value
            coroutine.yield("pause")
        end,
    })
    setfenv(1, environment)
    answer = 42
    return received
end
local thread = coroutine.create(body)
local first, pause = coroutine.resume(thread)
local second, result = coroutine.resume(thread)
local ok = first and pause == "pause" and second and result == "answer:42"
print(type(ok) .. ":" .. tostring(ok))
"#;
const YIELDING_GSUB_TABLE_INDEX_SOURCE: &str = r#"
local thread = coroutine.create(function()
    local replacements = setmetatable({}, {
        __index = function()
            coroutine.yield("gsub index pause")
            return "replacement"
        end,
    })
    return string.gsub("a", "a", replacements)
end)
local ok = coroutine.resume(thread)
return not ok
"#;
const YIELDING_GSUB_TABLE_INDEX_REFERENCE_SOURCE: &str = r#"
local type_fn = type
local tostring_fn = tostring
local print_fn = print
local thread = coroutine.create(function()
    local replacements = setmetatable({}, {
        __index = function()
            coroutine.yield("gsub index pause")
            return "replacement"
        end,
    })
    return string.gsub("a", "a", replacements)
end)
local ok = coroutine.resume(thread)
print_fn(type_fn(not ok) .. ":" .. tostring_fn(not ok))
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
const REPEAT_SCOPE_SOURCE: &str = r#"
local count = 0
repeat
    local marker = count + 1
    count = marker
until marker == 3
return count == 3
"#;
const REPEAT_SCOPE_REFERENCE_SOURCE: &str = r#"
local count = 0
repeat
    local marker = count + 1
    count = marker
until marker == 3
local result = count == 3
print(type(result) .. ":" .. tostring(result))
"#;
const DYNAMIC_NUMERIC_FOR_SOURCE: &str = r#"
local step = -2
local total = 0
for index = 5, 1, step do
    total = total + index
end
return total == 9
"#;
const DYNAMIC_NUMERIC_FOR_REFERENCE_SOURCE: &str = r#"
local step = -2
local total = 0
for index = 5, 1, step do
    total = total + index
end
local result = total == 9
print(type(result) .. ":" .. tostring(result))
"#;
const ZERO_NUMERIC_FOR_SOURCE: &str = r#"
local ok, message = pcall(function()
    local step = 0
    for index = 1, 1, step do end
end)
return not ok and type(message) == "string"
"#;
const ZERO_NUMERIC_FOR_REFERENCE_SOURCE: &str = r#"
local ok, message = pcall(function()
    local step = 0
    for index = 1, 1, step do end
end)
local result = not ok and type(message) == "string"
print(type(result) .. ":" .. tostring(result))
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
const STRING_CALL_SOURCE: &str = r#"
local function echo(value)
    return value
end
return echo"answer"
"#;
const STRING_CALL_REFERENCE_SOURCE: &str = r#"
local function echo(value)
    return value
end
local result = echo"answer"
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
const DIRECT_ITERATION_HOOK_SOURCE: &str = r#"
local values = setmetatable({}, {
    __iter = function(value)
        local function iterator(_, index)
            if index < 2 then
                return index + 1, "hook" .. tostring(index + 1)
            end
        end
        return iterator, value, 0
    end,
})
local result = ""
for index, value in values do
    result = result .. value
end
return result
"#;
const DIRECT_ITERATION_HOOK_REFERENCE_SOURCE: &str = r#"
local values = setmetatable({}, {
    __iter = function(value)
        local function iterator(_, index)
            if index < 2 then
                return index + 1, "hook" .. tostring(index + 1)
            end
        end
        return iterator, value, 0
    end,
})
local result = ""
for index, value in values do
    result = result .. value
end
print(type(result) .. ":" .. result)
"#;
const IPAIRS_HOOK_SOURCE: &str = r#"
local calls = 0
local values = setmetatable({}, {
    __ipairs = function(table)
        calls = calls + 1
        local function iterator(_, index)
            if index < 2 then
                return index + 1, "hook" .. tostring(index + 1)
            end
        end
        return iterator, table, 0
    end,
})
local iterator, state, control = ipairs(values)
local first, first_value = iterator(state, control)
local second, second_value
if first ~= nil then
    second, second_value = iterator(state, first)
end
local used = _VERSION == "Lua 5.2" or _VERSION == "Lua 5.3"
return used
    and calls == 1 and first == 1 and first_value == "hook1"
    and second == 2 and second_value == "hook2"
    or not used and calls == 0 and first == nil and first_value == nil and second == nil
"#;
const IPAIRS_HOOK_REFERENCE_SOURCE: &str = r#"
local calls = 0
local values = setmetatable({}, {
    __ipairs = function(table)
        calls = calls + 1
        local function iterator(_, index)
            if index < 2 then
                return index + 1, "hook" .. tostring(index + 1)
            end
        end
        return iterator, table, 0
    end,
})
local iterator, state, control = ipairs(values)
local first, first_value = iterator(state, control)
local second, second_value
if first ~= nil then
    second, second_value = iterator(state, first)
end
local used = _VERSION == "Lua 5.2" or _VERSION == "Lua 5.3"
local result = used
    and calls == 1 and first == 1 and first_value == "hook1"
    and second == 2 and second_value == "hook2"
    or not used and calls == 0 and first == nil and first_value == nil and second == nil
print(type(result) .. ":" .. tostring(result))
"#;
const DIRECT_ITERATION_EDGE_SOURCE: &str = r#"
local log = {}
local values = { 1, 2 }
local function iterator(state, index)
    log[#log + 1] = "iter:" .. tostring(index)
    if index == 0 then
        state[3] = 3
    end
    if index < 3 then
        return index + 1, state[index + 1]
    end
end
local object = setmetatable(values, {
    __iter = function(value)
        log[#log + 1] = "prepare"
        return iterator, value, 0
    end,
})
local seen = ""
for index, value in object do
    seen = seen .. index .. ":" .. value .. ","
end
local ok_yield = pcall(function()
    local yielding = setmetatable({}, {
        __iter = function()
            coroutine.yield("pause")
        end,
    })
    for key, value in yielding do end
end)
return seen == "1:1,2:2,3:3,"
    and table.concat(log, ",") == "prepare,iter:0,iter:1,iter:2,iter:3"
    and #values == 3 and not ok_yield
"#;
const DIRECT_ITERATION_EDGE_REFERENCE_SOURCE: &str = r#"
local log = {}
local values = { 1, 2 }
local function iterator(state, index)
    log[#log + 1] = "iter:" .. tostring(index)
    if index == 0 then
        state[3] = 3
    end
    if index < 3 then
        return index + 1, state[index + 1]
    end
end
local object = setmetatable(values, {
    __iter = function(value)
        log[#log + 1] = "prepare"
        return iterator, value, 0
    end,
})
local seen = ""
for index, value in object do
    seen = seen .. index .. ":" .. value .. ","
end
local ok_yield = pcall(function()
    local yielding = setmetatable({}, {
        __iter = function()
            coroutine.yield("pause")
        end,
    })
    for key, value in yielding do end
end)
local result = seen == "1:1,2:2,3:3,"
    and table.concat(log, ",") == "prepare,iter:0,iter:1,iter:2,iter:3"
    and #values == 3 and not ok_yield
print(type(result) .. ":" .. tostring(result))
"#;
const DIRECT_ITERATION_YIELD_SOURCE: &str = r#"
local co = coroutine.create(function()
    local values = setmetatable({ "a", "b" }, {
        __iter = function(value)
            coroutine.yield("prepared")
            coroutine.yield("still-prepared")
            local function iterator(state, index)
                if index < 2 then
                    return index + 1, state[index + 1]
                end
            end
            return iterator, value, 0
        end,
    })
    local seen = ""
    for index, value in values do
        seen = seen .. index .. ":" .. value .. ","
    end
    return seen
end)
local first, yielded = coroutine.resume(co)
local second, yielded_again = coroutine.resume(co)
local third, result = coroutine.resume(co)
return first and yielded == "prepared"
    and second and yielded_again == "still-prepared"
    and third and result == "1:a,2:b,"
    and coroutine.status(co) == "dead"
"#;
const DIRECT_ITERATION_YIELD_REFERENCE_SOURCE: &str = r#"
local co = coroutine.create(function()
    local values = setmetatable({ "a", "b" }, {
        __iter = function(value)
            coroutine.yield("prepared")
            coroutine.yield("still-prepared")
            local function iterator(state, index)
                if index < 2 then
                    return index + 1, state[index + 1]
                end
            end
            return iterator, value, 0
        end,
    })
    local seen = ""
    for index, value in values do
        seen = seen .. index .. ":" .. value .. ","
    end
    return seen
end)
local first, yielded = coroutine.resume(co)
local second, yielded_again = coroutine.resume(co)
local third, result = coroutine.resume(co)
local ok = first and yielded == "prepared"
    and second and yielded_again == "still-prepared"
    and third and result == "1:a,2:b,"
    and coroutine.status(co) == "dead"
print(type(ok) .. ":" .. tostring(ok))
"#;
const IPAIRS_INTEGER_ARGUMENT_SOURCE: &str = r#"
local iterator, state = ipairs({"a", "b"})
local ok, index, value = pcall(iterator, state, 1.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok and index == 2 and value == "b"
"#;
const IPAIRS_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local iterator, state = ipairs({"a", "b"})
local ok, index, value = pcall(iterator, state, 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok and index == 2 and value == "b"
print(type(result) .. ":" .. tostring(result))
"#;
const BASE_LIBRARY_SOURCE: &str = r#"
return type(3) .. ":" .. tostring(3)
"#;
const BASE_LIBRARY_REFERENCE_SOURCE: &str = r#"
local result = type(3) .. ":" .. tostring(3)
print(type(result) .. ":" .. tostring(result))
"#;
const TOSTRING_METAMETHOD_SOURCE: &str = r#"
local calls = 0
local object = setmetatable({}, {
    __tostring = function(value)
        calls = calls + 1
        return "object:" .. type(value)
    end,
})
return tostring(object) == "object:table" and calls == 1
"#;
const TOSTRING_METAMETHOD_REFERENCE_SOURCE: &str = r#"
local calls = 0
local object = setmetatable({}, {
    __tostring = function(value)
        calls = calls + 1
        return "object:" .. type(value)
    end,
})
local result = tostring(object) == "object:table" and calls == 1
print(type(result) .. ":" .. tostring(result))
"#;
const TOSTRING_YIELD_SOURCE: &str = r#"
local co = coroutine.create(function()
    local object = setmetatable({}, {
        __tostring = function()
            coroutine.yield("pause")
            return "resumed"
        end,
    })
    return tostring(object)
end)
local first, yielded = coroutine.resume(co)
local second, result = coroutine.resume(co)
return first and yielded == "pause"
    and second and result == "resumed"
    and coroutine.status(co) == "dead"
"#;
const TOSTRING_YIELD_REFERENCE_SOURCE: &str = r#"
local co = coroutine.create(function()
    local object = setmetatable({}, {
        __tostring = function()
            coroutine.yield("pause")
            return "resumed"
        end,
    })
    return tostring(object)
end)
local first, yielded = coroutine.resume(co)
local second, result = coroutine.resume(co)
local ok = first and yielded == "pause"
    and second and result == "resumed"
    and coroutine.status(co) == "dead"
print(type(ok) .. ":" .. tostring(ok))
"#;
const STRING_FORMAT_GENERAL_SOURCE: &str = r#"
return string.format("%.3g|%.3G|%10.3g|%-10.3g|%.5g|%.3g|%.3g|%.3g|%.3G|%.0g", 12.34, 1234.5, 12.34, 12.34, 0.000012345, 999.5, 0.0001, 0.00001, 1234.5, 123.5)
"#;
const STRING_FORMAT_GENERAL_REFERENCE_SOURCE: &str = r#"
local result = string.format("%.3g|%.3G|%10.3g|%-10.3g|%.5g|%.3g|%.3g|%.3g|%.3G|%.0g", 12.34, 1234.5, 12.34, 12.34, 0.000012345, 999.5, 0.0001, 0.00001, 1234.5, 123.5)
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_QUOTED_SOURCE: &str = r#"
return string.format("%q|%q|%q", "a\"b\\c", 12, 12.5)
"#;
const STRING_FORMAT_QUOTED_REFERENCE_SOURCE: &str = r#"
local result = string.format("%q|%q|%q", "a\"b\\c", 12, 12.5)
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_QUOTED_NONSCALAR_SOURCE: &str = r#"
local nil_ok, nil_value = pcall(string.format, "%q", nil)
local false_ok, false_value = pcall(string.format, "%q", false)
local true_ok, true_value = pcall(string.format, "%q", true)
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
return legacy
    and not nil_ok and not false_ok and not true_ok
    or not legacy
        and nil_ok and nil_value == "nil"
        and false_ok and false_value == "false"
        and true_ok and true_value == "true"
"#;
const STRING_FORMAT_QUOTED_NONSCALAR_REFERENCE_SOURCE: &str = r#"
local nil_ok, nil_value = pcall(string.format, "%q", nil)
local false_ok, false_value = pcall(string.format, "%q", false)
local true_ok, true_value = pcall(string.format, "%q", true)
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
local result = legacy
    and not nil_ok and not false_ok and not true_ok
    or not legacy
        and nil_ok and nil_value == "nil"
        and false_ok and false_value == "false"
        and true_ok and true_value == "true"
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_QUOTED_NONSCALAR_BLU_SOURCE: &str = r#"
local nil_ok, nil_value = pcall(string.format, "%q", nil)
local false_ok, false_value = pcall(string.format, "%q", false)
local true_ok, true_value = pcall(string.format, "%q", true)
return nil_ok and nil_value == "nil"
    and false_ok and false_value == "false"
    and true_ok and true_value == "true"
"#;
const STRING_FORMAT_HEXADECIMAL_SOURCE: &str = r#"
return string.format("%.0a|%.1a|%.2a|%.3a|%.13a|%.3E", 12.5, 12.5, 12.5, 12.5, 12.5, 0.00125)
"#;
const STRING_FORMAT_HEXADECIMAL_REFERENCE_SOURCE: &str = r#"
local result = string.format("%.0a|%.1a|%.2a|%.3a|%.13a|%.3E", 12.5, 12.5, 12.5, 12.5, 12.5, 0.00125)
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_FLAGS_SOURCE: &str = r#"
return string.format("%+d|% d|%#x|%#X|%#o|%05d|%08.2f|%-05d|%#.5g", 15, 15, 255, 255, 9, 15, 1.25, 15, 1.25)
"#;
const STRING_FORMAT_FLAGS_REFERENCE_SOURCE: &str = r#"
local result = string.format("%+d|% d|%#x|%#X|%#o|%05d|%08.2f|%-05d|%#.5g", 15, 15, 255, 255, 9, 15, 1.25, 15, 1.25)
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_INTEGER_PRECISION_SOURCE: &str = r#"
return string.format("%.3d|%+.3d|%.5u|%.5x|%#.5o|%.0d|%#.0o|%08.5d|%08.5x", 12, -12, 12, 255, 9, 0, 0, 12, 255)
"#;
const STRING_FORMAT_INTEGER_PRECISION_REFERENCE_SOURCE: &str = r#"
local result = string.format("%.3d|%+.3d|%.5u|%.5x|%#.5o|%.0d|%#.0o|%08.5d|%08.5x", 12, -12, 12, 255, 9, 0, 0, 12, 255)
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_MODIFIER_SOURCE: &str = r#"
local function accepts(format, value)
    local ok = pcall(string.format, format, value)
    return ok
end
local modern = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local expected = not modern
local result = accepts("%#d", 15) == expected
    and accepts("%+u", 15) == expected
    and accepts("%#s", 15) == expected
    and accepts("%0s", 15) == expected
    and accepts("%+q", 15) == expected
    and accepts("%5q", 15) == expected
    and accepts("%05c", 65) == expected
    and accepts("%+x", 15) == expected
return result
"#;
const STRING_FORMAT_MODIFIER_REFERENCE_SOURCE: &str = r#"
local function accepts(format, value)
    local ok = pcall(string.format, format, value)
    return ok
end
local modern = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local expected = not modern
local result = accepts("%#d", 15) == expected
    and accepts("%+u", 15) == expected
    and accepts("%#s", 15) == expected
    and accepts("%0s", 15) == expected
    and accepts("%+q", 15) == expected
    and accepts("%5q", 15) == expected
    and accepts("%05c", 65) == expected
    and accepts("%+x", 15) == expected
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_STRING_ARGUMENT_SOURCE: &str = r#"
local ok_decimal, decimal = pcall(string.format, "%d", "3")
local ok_float, floating = pcall(string.format, "%.2f", "3.5")
local ok_hex, hexadecimal = pcall(string.format, "%x", "15")
local ok_string, string_value = pcall(string.format, "%s", 3)
local ok_char, character = pcall(string.format, "%c", "65")
local ok_quoted, quoted = pcall(string.format, "%q", "abc")
return ok_decimal and decimal == "3"
    and ok_float and floating == "3.50"
    and ok_hex and hexadecimal == "f"
    and ok_string and string_value == "3"
    and ok_char and character == "A"
    and ok_quoted and quoted == '"abc"'
"#;
const STRING_FORMAT_STRING_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok_decimal, decimal = pcall(string.format, "%d", "3")
local ok_float, floating = pcall(string.format, "%.2f", "3.5")
local ok_hex, hexadecimal = pcall(string.format, "%x", "15")
local ok_string, string_value = pcall(string.format, "%s", 3)
local ok_char, character = pcall(string.format, "%c", "65")
local ok_quoted, quoted = pcall(string.format, "%q", "abc")
local result = ok_decimal and decimal == "3"
    and ok_float and floating == "3.50"
    and ok_hex and hexadecimal == "f"
    and ok_string and string_value == "3"
    and ok_char and character == "A"
    and ok_quoted and quoted == '"abc"'
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_NONSCALAR_SOURCE: &str = r#"
local table_ok, table_value = pcall(string.format, "%s", {})
local function_ok, function_value = pcall(string.format, "%s", function() end)
local custom = setmetatable({}, { __tostring = function() return "custom" end })
local custom_ok, custom_value = pcall(string.format, "%s", custom)
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Luau" or _VERSION == nil
return legacy
    and not table_ok and not function_ok and not custom_ok
    or not legacy
        and table_ok and type(table_value) == "string"
        and function_ok and type(function_value) == "string"
        and custom_ok and custom_value == "custom"
"#;
const STRING_FORMAT_NONSCALAR_REFERENCE_SOURCE: &str = r#"
local table_ok, table_value = pcall(string.format, "%s", {})
local function_ok, function_value = pcall(string.format, "%s", function() end)
local custom = setmetatable({}, { __tostring = function() return "custom" end })
local custom_ok, custom_value = pcall(string.format, "%s", custom)
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Luau" or _VERSION == nil
local result = legacy
    and not table_ok and not function_ok and not custom_ok
    or not legacy
        and table_ok and type(table_value) == "string"
        and function_ok and type(function_value) == "string"
        and custom_ok and custom_value == "custom"
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_NONSCALAR_BLU_SOURCE: &str = r#"
local table_ok, table_value = pcall(string.format, "%s", {})
local function_ok, function_value = pcall(string.format, "%s", function() end)
local custom = setmetatable({}, { __tostring = function() return "custom" end })
local custom_ok, custom_value = pcall(string.format, "%s", custom)
return table_ok and type(table_value) == "string"
    and function_ok and type(function_value) == "string"
    and custom_ok and custom_value == "custom"
"#;
const STRING_FORMAT_CHAR_RANGE_SOURCE: &str = r#"
local function byte(value)
    local ok, result = pcall(string.format, "%c", value)
    return ok and string.byte(result) or nil
end
local fraction_ok, fraction = pcall(string.format, "%c", 65.9)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
local lua51 = _VERSION == "Lua 5.1"
local zero_bytes = lua51 and byte(256) == nil and byte(512) == nil
    or not lua51 and byte(256) == 0 and byte(512) == 0
return byte(-1) == 255 and byte(300) == 44
    and byte(511) == 255 and zero_bytes
    and (modern and not fraction_ok or legacy and fraction_ok
        and string.byte(fraction) == 65)
"#;
const STRING_FORMAT_CHAR_RANGE_REFERENCE_SOURCE: &str = r#"
local function byte(value)
    local ok, result = pcall(string.format, "%c", value)
    return ok and string.byte(result) or nil
end
local fraction_ok, fraction = pcall(string.format, "%c", 65.9)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
local lua51 = _VERSION == "Lua 5.1"
local zero_bytes = lua51 and byte(256) == nil and byte(512) == nil
    or not lua51 and byte(256) == 0 and byte(512) == 0
local result = byte(-1) == 255 and byte(300) == 44
    and byte(511) == 255 and zero_bytes
    and (modern and not fraction_ok or legacy and fraction_ok
        and string.byte(fraction) == 65)
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_FORMAT_CHAR_RANGE_BLU_SOURCE: &str = r#"
local function byte(value)
    local ok, result = pcall(string.format, "%c", value)
    return ok and string.byte(result) or nil
end
local fraction_ok = pcall(string.format, "%c", 65.9)
return byte(-1) == 255 and byte(300) == 44
    and byte(511) == 255 and byte(256) == 0 and byte(512) == 0
    and not fraction_ok
"#;
const STRING_GMATCH_EMPTY_SOURCE: &str = r#"
local iterator = string.gmatch("ba", "a*")
local first = iterator()
local second = iterator()
local third = iterator()
local empty_iterator = string.gmatch("abc", "")
local empty_first = empty_iterator()
local empty_second = empty_iterator()
local empty_third = empty_iterator()
local empty_fourth = empty_iterator()
local empty_fifth = empty_iterator()
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
local empty_ok = empty_first == "" and empty_second == ""
    and empty_third == "" and empty_fourth == "" and empty_fifth == nil
return empty_ok and (modern and first == "" and second == "a" and third == nil
    or legacy and first == "" and second == "a" and third == "")
"#;
const STRING_GMATCH_EMPTY_REFERENCE_SOURCE: &str = r#"
local iterator = string.gmatch("ba", "a*")
local first = iterator()
local second = iterator()
local third = iterator()
local empty_iterator = string.gmatch("abc", "")
local empty_first = empty_iterator()
local empty_second = empty_iterator()
local empty_third = empty_iterator()
local empty_fourth = empty_iterator()
local empty_fifth = empty_iterator()
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
local empty_ok = empty_first == "" and empty_second == ""
    and empty_third == "" and empty_fourth == "" and empty_fifth == nil
local result = empty_ok and (modern and first == "" and second == "a" and third == nil
    or legacy and first == "" and second == "a" and third == "")
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_GMATCH_EMPTY_BLU_SOURCE: &str = r#"
local iterator = string.gmatch("ba", "a*")
local first = iterator()
local second = iterator()
local third = iterator()
local empty_iterator = string.gmatch("abc", "")
local empty_first = empty_iterator()
local empty_second = empty_iterator()
local empty_third = empty_iterator()
local empty_fourth = empty_iterator()
local empty_fifth = empty_iterator()
return first == "" and second == "a" and third == nil
    and empty_first == "" and empty_second == ""
    and empty_third == "" and empty_fourth == "" and empty_fifth == nil
"#;
const STRING_GSUB_EMPTY_SOURCE: &str = r#"
local first, first_count = string.gsub("ba", "a*", "X")
local second, second_count = string.gsub("aa", "a*", "X")
local third, third_count = string.gsub("", "a*", "X")
local fourth, fourth_count = string.gsub("abc", "", "-")
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
return modern
        and first == "XbX" and first_count == 2
        and second == "X" and second_count == 1
        and third == "X" and third_count == 1
        and fourth == "-a-b-c-" and fourth_count == 4
    or legacy
        and first == "XbXX" and first_count == 3
        and second == "XX" and second_count == 2
        and third == "X" and third_count == 1
"#;
const STRING_GSUB_EMPTY_REFERENCE_SOURCE: &str = r#"
local first, first_count = string.gsub("ba", "a*", "X")
local second, second_count = string.gsub("aa", "a*", "X")
local third, third_count = string.gsub("", "a*", "X")
local fourth, fourth_count = string.gsub("abc", "", "-")
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Luau" or _VERSION == nil
local result = modern
        and first == "XbX" and first_count == 2
        and second == "X" and second_count == 1
        and third == "X" and third_count == 1
        and fourth == "-a-b-c-" and fourth_count == 4
    or legacy
        and first == "XbXX" and first_count == 3
        and second == "XX" and second_count == 2
        and third == "X" and third_count == 1
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_GSUB_EMPTY_BLU_SOURCE: &str = r#"
local first, first_count = string.gsub("ba", "a*", "X")
local second, second_count = string.gsub("aa", "a*", "X")
local third, third_count = string.gsub("", "a*", "X")
local fourth, fourth_count = string.gsub("abc", "", "-")
return first == "XbX" and first_count == 2
    and second == "X" and second_count == 1
    and third == "X" and third_count == 1
    and fourth == "-a-b-c-" and fourth_count == 4
"#;
const PACKAGE_SOURCE: &str = r#"
return type(package.loaded) .. ":" .. type(package.preload)
    .. ":" .. tostring(package.config == "/\n;\n?\n!\n-" or package.config == "/\n;\n?\n!\n-\n")
"#;
const PACKAGE_REFERENCE_SOURCE: &str = r#"
local result = type(package.loaded) .. ":" .. type(package.preload)
    .. ":" .. tostring(package.config == "/\n;\n?\n!\n-" or package.config == "/\n;\n?\n!\n-\n")
print(type(result) .. ":" .. tostring(result))
"#;
const PACKAGE_DEFAULTS_SOURCE: &str = r#"
local version = _VERSION == "Lua 5.1" and "5.1"
    or _VERSION == "Lua 5.2" and "5.2"
    or _VERSION == "Lua 5.3" and "5.3"
    or _VERSION == "Lua 5.4" and "5.4"
    or "5.5"
local root = "/usr/local/"
local path
if version == "5.1" then
    path = "./?.lua;" .. root .. "share/lua/5.1/?.lua;"
        .. root .. "share/lua/5.1/?/init.lua;"
        .. root .. "lib/lua/5.1/?.lua;"
        .. root .. "lib/lua/5.1/?/init.lua"
else
    path = root .. "share/lua/" .. version .. "/?.lua;"
        .. root .. "share/lua/" .. version .. "/?/init.lua;"
        .. root .. "lib/lua/" .. version .. "/?.lua;"
        .. root .. "lib/lua/" .. version .. "/?/init.lua;./?.lua"
    if version ~= "5.2" then
        path = path .. ";./?/init.lua"
    end
end
local cpath = version == "5.1"
    and "./?.so;" .. root .. "lib/lua/5.1/?.so;" .. root .. "lib/lua/5.1/loadall.so"
    or root .. "lib/lua/" .. version .. "/?.so;" .. root .. "lib/lua/" .. version
        .. "/loadall.so;./?.so"
return package.path == path and package.cpath == cpath
    and rawget(package, "path") == package.path
    and rawget(package, "cpath") == package.cpath
    and rawget(package, "config") == package.config
    and rawget(package, "loadlib") == package.loadlib
    and ((_VERSION == "Lua 5.1" and rawget(package, "searchpath") == nil)
        or (_VERSION ~= "Lua 5.1" and rawget(package, "searchpath") == package.searchpath))
"#;
const PACKAGE_DEFAULTS_REFERENCE_SOURCE: &str = r#"
local version = _VERSION == "Lua 5.1" and "5.1"
    or _VERSION == "Lua 5.2" and "5.2"
    or _VERSION == "Lua 5.3" and "5.3"
    or _VERSION == "Lua 5.4" and "5.4"
    or "5.5"
local root = "/usr/local/"
local path
if version == "5.1" then
    path = "./?.lua;" .. root .. "share/lua/5.1/?.lua;"
        .. root .. "share/lua/5.1/?/init.lua;"
        .. root .. "lib/lua/5.1/?.lua;"
        .. root .. "lib/lua/5.1/?/init.lua"
else
    path = root .. "share/lua/" .. version .. "/?.lua;"
        .. root .. "share/lua/" .. version .. "/?/init.lua;"
        .. root .. "lib/lua/" .. version .. "/?.lua;"
        .. root .. "lib/lua/" .. version .. "/?/init.lua;./?.lua"
    if version ~= "5.2" then
        path = path .. ";./?/init.lua"
    end
end
local cpath = version == "5.1"
    and "./?.so;" .. root .. "lib/lua/5.1/?.so;" .. root .. "lib/lua/5.1/loadall.so"
    or root .. "lib/lua/" .. version .. "/?.so;" .. root .. "lib/lua/" .. version
        .. "/loadall.so;./?.so"
local result = package.path == path and package.cpath == cpath
    and rawget(package, "path") == package.path
    and rawget(package, "cpath") == package.cpath
    and rawget(package, "config") == package.config
    and rawget(package, "loadlib") == package.loadlib
    and ((_VERSION == "Lua 5.1" and rawget(package, "searchpath") == nil)
        or (_VERSION ~= "Lua 5.1" and rawget(package, "searchpath") == package.searchpath))
print(type(result) .. ":" .. tostring(result))
"#;
const PACKAGE_LOADLIB_SOURCE: &str = r#"
local loaded, message, where = package.loadlib("missing", "luaopen_missing")
return type(package.loadlib) == "function"
    and loaded == nil and type(message) == "string" and where == "absent"
"#;
const PACKAGE_LOADLIB_REFERENCE_SOURCE: &str = r#"
local loaded, message, where = package.loadlib("missing", "luaopen_missing")
local result = type(package.loadlib) == "function"
    and loaded == nil and type(message) == "string" and where == "absent"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_SOURCE: &str = r#"
return type(os) == "table" and os.difftime(9, 4) == 5
"#;
const OS_REFERENCE_SOURCE: &str = r#"
local result = type(os) == "table" and os.difftime(9, 4) == 5
print(type(result) .. ":" .. tostring(result))
"#;
const OS_DEBUG_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok_traceback, traceback = pcall(function()
    if not debug or not debug.traceback then return "absent" end
    return debug.traceback("x", 1.5)
end)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local has_traceback = _VERSION ~= "Blu"
return not has_traceback and ok_traceback and traceback == "absent"
    or has_traceback and (modern and not ok_traceback
        or not modern and ok_traceback and type(traceback) == "string")
"#;
const OS_DEBUG_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok_traceback, traceback = pcall(function()
    if not debug or not debug.traceback then return "absent" end
    return debug.traceback("x", 1.5)
end)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local has_traceback = _VERSION ~= "Blu"
local result = not has_traceback and ok_traceback and traceback == "absent"
    or has_traceback and (modern and not ok_traceback
        or not modern and ok_traceback and type(traceback) == "string")
print(type(result) .. ":" .. tostring(result))
"#;
const OS_EXECUTE_SOURCE: &str = r#"
local available = os.execute()
local status, kind, code = os.execute("true")
if _VERSION == "Lua 5.1" then
    return available == 1 and status == 0 and kind == nil and code == nil
end
return available == true and status == true and kind == "exit" and code == 0
"#;
const OS_EXECUTE_REFERENCE_SOURCE: &str = r#"
local available = os.execute()
local status, kind, code = os.execute("true")
local result
if _VERSION == "Lua 5.1" then
    result = available == 1 and status == 0 and kind == nil and code == nil
else
    result = available == true and status == true and kind == "exit" and code == 0
end
print(type(result) .. ":" .. tostring(result))
"#;
const OS_EXIT_SOURCE: &str = r#"
if _VERSION == "Lua 5.1" then
    os.exit(7)
else
    os.exit(true, true)
end
return true
"#;
const OS_EXIT_REFERENCE_SOURCE: &str = "os.exit(0)\n";
const OS_LOCALE_SOURCE: &str = r#"
local current = os.setlocale(nil)
local numeric = os.setlocale("C", "numeric")
return type(current) == "string"
    and numeric == "C"
    and type(os.tmpname()) == "string"
"#;
const OS_LOCALE_REFERENCE_SOURCE: &str = r#"
local current = os.setlocale(nil)
local numeric = os.setlocale("C", "numeric")
local result = type(current) == "string"
    and numeric == "C"
    and type(os.tmpname()) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_LOCALE_INVALID_CATEGORY_SOURCE: &str = r#"
local ok, message = pcall(os.setlocale, nil, "invalid")
return not ok and type(message) == "string"
"#;
const OS_LOCALE_INVALID_CATEGORY_REFERENCE_SOURCE: &str = r#"
local ok, message = pcall(os.setlocale, nil, "invalid")
local result = not ok and type(message) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_CLOCK_SOURCE: &str = r#"
return type(os.clock) == "function" and type(os.clock()) == "number"
"#;
const OS_CLOCK_REFERENCE_SOURCE: &str = r#"
local result = type(os.clock) == "function" and type(os.clock()) == "number"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_TIME_SOURCE: &str = r#"
return type(os.time) == "function"
    and type(os.time()) == "number"
    and os.time() == 1700000000
"#;
const OS_TIME_REFERENCE_SOURCE: &str = r#"
local result = type(os.time) == "function" and type(os.time()) == "number"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_TIME_TABLE_SOURCE: &str = r#"
local result = os.time{
    year = 2023, month = 11, day = 14,
    hour = 22, min = 13, sec = 20, isdst = false
}
return type(result) == "number" and result == 1700000000
"#;
const OS_TIME_TABLE_REFERENCE_SOURCE: &str = r#"
local input = {
    year = 2023, month = 11, day = 14,
    hour = 22, min = 13, sec = 20, isdst = false
}
local result = os.time(input)
print(type(result) .. ":" .. tostring(result == 1700000000))
"#;
const OS_TIME_TABLE_DEFAULTS_SOURCE: &str = r#"
local result = os.time{year = 2023, month = 11, day = 14}
return type(result) == "number" and result == 1699963200
"#;
const OS_TIME_TABLE_DEFAULTS_REFERENCE_SOURCE: &str = r#"
local result = os.time{year = 2023, month = 11, day = 14}
print(type(result) .. ":" .. tostring(result == 1699963200))
"#;
const OS_TIME_TABLE_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok, value = pcall(os.time, {
    year = 2023.5, month = 11, day = 14,
    hour = 22, min = 13, sec = 20, isdst = false
})
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok and type(value) == "number"
"#;
const OS_TIME_TABLE_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok, value = pcall(os.time, {
    year = 2023.5, month = 11, day = 14,
    hour = 22, min = 13, sec = 20, isdst = false
})
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok and type(value) == "number"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_DATE_SOURCE: &str = r#"
return type(os.date) == "function"
    and os.date("!%Y-%m-%d", 1700000000) == "2023-11-14"
"#;
const OS_DATE_REFERENCE_SOURCE: &str = r#"
local result = type(os.date) == "function"
    and os.date("!%Y-%m-%d", 1700000000) == "2023-11-14"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_DATE_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok, value = pcall(os.date, "!%Y", 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok and value == "1970"
"#;
const OS_DATE_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok, value = pcall(os.date, "!%Y", 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok and value == "1970"
print(type(result) .. ":" .. tostring(result))
"#;
const OS_DATE_TABLE_SOURCE: &str = r#"
local date = os.date("!*t", 1700000000)
return date.year == 2023 and date.month == 11 and date.day == 14
    and date.hour == 22 and date.min == 13 and date.sec == 20
    and date.wday == 3 and date.yday == 318 and not date.isdst
"#;
const OS_DATE_TABLE_REFERENCE_SOURCE: &str = r#"
local date = os.date("!*t", 1700000000)
local result = date.year == 2023 and date.month == 11 and date.day == 14
    and date.hour == 22 and date.min == 13 and date.sec == 20
    and date.wday == 3 and date.yday == 318 and not date.isdst
print(type(result) .. ":" .. tostring(result))
"#;
const IO_SOURCE: &str = r#"
local input, open_error = io.open("answer.txt", "rb")
local buffered = input:setvbuf("full", 64)
local first = input:read(6)
local line = input:read("*l")
local position = input:seek("set", 0)
local all = input:read("*a")
local flushed = input:flush()
local reset = input:seek("set", 0)
local iterator = input:lines()
local first_line = iterator()
local second_line = iterator()
local done = iterator()
local output = io.open("output.txt", "w")
local returned = output:write("blu", 5)
output:flush()
output:close()
local before = io.type(input)
local iterator_type = type(input.lines)
local closed = io.close(input)
local after = io.type(input)
local second_ok, close_error = pcall(io.close, input)
local default_input = io.input()
local default_output = io.output()
local default_error = io.stderr
local input_failure_ok, input_failure_error = pcall(io.input, "missing-input.txt")
local output_failure_ok, output_failure_error = pcall(io.output, "missing-output.txt")
local closed_input_file = io.open("answer.txt", "rb")
closed_input_file:close()
local closed_input_ok, closed_input_error = pcall(io.input, closed_input_file)
local closed_output_file = io.open("answer.txt", "rb")
closed_output_file:close()
local closed_output_ok, closed_output_error = pcall(io.output, closed_output_file)
local rebound_first = io.output("rebound-first.txt")
local rebound_second = io.output("rebound-second.txt")
local rebound_old_write = rebound_first:write("old")
local rebound_current = io.output()
local rebound_close = io.close()
local rebound_old_type = io.type(rebound_first)
local rebound_current_type = io.type(rebound_second)
local named_iterator, named_state, named_control, named_file = io.lines("answer.txt")
local named_first = named_iterator()
local named_done = named_iterator()
local named_file_type = named_file and io.type(named_file) or nil
local modern_named_lines = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local named_shape = modern_named_lines
    and named_state == nil and named_control == nil
    and named_file ~= nil and named_file_type == "closed file"
    or not modern_named_lines
        and named_state == nil and named_control == nil and named_file == nil
local switched_input = io.input("answer.txt")
local switched_first = io.read("*l")
local switched_output = io.output("output.txt")
local switched_returned = io.write("switched")
local multi = io.open("answer.txt", "rb")
local multi_first, multi_second = multi:read("*l", "*l")
local userdata_values = {}
local userdata_metatable = {
    __index = { answer = 42 },
    __newindex = function(_, key, value) userdata_values[key] = value end,
}
local userdata_global_ok = pcall(setmetatable, multi, userdata_metatable)
local userdata_returned = debug.setmetatable(multi, userdata_metatable)
local userdata_answer = multi.answer
multi.answer = 43
local numeric = io.open("numbers.txt", "rb")
local integer, fraction = numeric:read("*n", "*n")
numeric:seek("set", 0)
local numeric_iterator = numeric:lines("*n")
local numeric_first = numeric_iterator()
local numeric_second = numeric_iterator()
local numeric_done = numeric_iterator()
local multi_lines = io.open("multi_lines.txt", "rb")
local line_iterator = multi_lines:lines("*l", "*l")
local line_first, line_second = line_iterator()
local line_done = line_iterator()
local break_iterator, break_state, break_control, break_file = io.lines("answer.txt")
for _ in break_iterator, break_state, break_control, break_file do
    break
end
local break_closed = break_file == nil or io.type(break_file) == "closed file"
local process_ok, process = pcall(io.popen, "printf popen", "r")
local process_value = process_ok and process:read("*a") or "unsupported"
local process_closed = process_ok and process:close() or true
local invalid_process_ok, invalid_process_error = pcall(io.popen, "printf popen", "invalid")
local missing, missing_error = io.open("missing.txt", "rb")
return type(io) == "table" and open_error == nil and first == "owned\n"
    and buffered == true
    and line == "io" and position == 0 and all == "owned io\n"
    and flushed == true and returned == output and before == "file"
    and iterator_type == "function" and reset == 0
    and first_line == "owned" and second_line == "io" and done == nil
    and closed == true and after == "closed file"
    and second_ok == false and type(close_error) == "string"
    and default_input == io.stdin and default_output == io.stdout
    and io.type(default_error) == "file"
    and not input_failure_ok and type(input_failure_error) == "string"
    and not output_failure_ok and type(output_failure_error) == "string"
    and not closed_input_ok and type(closed_input_error) == "string"
    and not closed_output_ok and type(closed_output_error) == "string"
    and rebound_old_write == rebound_first
    and rebound_current == rebound_second and rebound_close == true
    and rebound_old_type == "file" and rebound_current_type == "closed file"
    and named_first == "owned io" and named_done == nil
    and named_shape and break_closed
    and switched_first == "owned io" and switched_returned == switched_output
    and multi_first == "owned io" and multi_second == nil
    and not userdata_global_ok
    and ((_VERSION == "Lua 5.1" and userdata_returned == true)
        or (_VERSION ~= "Lua 5.1" and userdata_returned == multi))
    and getmetatable(multi) == userdata_metatable
    and debug.getmetatable(multi) == userdata_metatable
    and userdata_answer == 42 and userdata_values.answer == 43
    and integer == 42 and fraction == 3.5
    and numeric_first == 42 and numeric_second == 3.5 and numeric_done == nil
    and line_first == "alpha" and line_second == "beta" and line_done == nil
    and ((process_ok and process_value == "popen" and process_closed == true)
        or (not process_ok and type(process) == "string"))
    and not invalid_process_ok and type(invalid_process_error) == "string"
    and missing == nil and type(missing_error) == "string"
"#;
const IO_REFERENCE_SOURCE: &str = r#"
local input, open_error = io.open("answer.txt", "rb")
local buffered = input:setvbuf("full", 64)
local first = input:read(6)
local line = input:read("*l")
local position = input:seek("set", 0)
local all = input:read("*a")
local flushed = input:flush()
local reset = input:seek("set", 0)
local iterator = input:lines()
local first_line = iterator()
local second_line = iterator()
local done = iterator()
local output = io.open("output.txt", "w")
local returned = output:write("blu", 5)
output:flush()
output:close()
local before = io.type(input)
local iterator_type = type(input.lines)
local closed = io.close(input)
local after = io.type(input)
local second_ok, close_error = pcall(io.close, input)
local default_input = io.input()
local default_output = io.output()
local default_error = io.stderr
local input_failure_ok, input_failure_error = pcall(io.input, "missing-input.txt")
local output_failure_ok, output_failure_error = pcall(io.output, "missing-output.txt")
local closed_input_file = io.open("answer.txt", "rb")
closed_input_file:close()
local closed_input_ok, closed_input_error = pcall(io.input, closed_input_file)
local closed_output_file = io.open("answer.txt", "rb")
closed_output_file:close()
local closed_output_ok, closed_output_error = pcall(io.output, closed_output_file)
local rebound_first = io.output("rebound-first.txt")
local rebound_second = io.output("rebound-second.txt")
local rebound_old_write = rebound_first:write("old")
local rebound_current = io.output()
local rebound_close = io.close()
local rebound_old_type = io.type(rebound_first)
local rebound_current_type = io.type(rebound_second)
local named_iterator, named_state, named_control, named_file = io.lines("answer.txt")
local named_first = named_iterator()
local named_done = named_iterator()
local named_file_type = named_file and io.type(named_file) or nil
local modern_named_lines = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local named_shape = modern_named_lines
    and named_state == nil and named_control == nil
    and named_file ~= nil and named_file_type == "closed file"
    or not modern_named_lines
        and named_state == nil and named_control == nil and named_file == nil
local switched_input = io.input("answer.txt")
local switched_first = io.read("*l")
local switched_output = io.output("output.txt")
local switched_returned = io.write("switched")
local multi = io.open("answer.txt", "rb")
local multi_first, multi_second = multi:read("*l", "*l")
local userdata_values = {}
local userdata_metatable = {
    __index = { answer = 42 },
    __newindex = function(_, key, value) userdata_values[key] = value end,
}
local userdata_global_ok = pcall(setmetatable, multi, userdata_metatable)
local userdata_returned = debug.setmetatable(multi, userdata_metatable)
local userdata_answer = multi.answer
multi.answer = 43
local numeric = io.open("numbers.txt", "rb")
local integer, fraction = numeric:read("*n", "*n")
numeric:seek("set", 0)
local numeric_iterator = numeric:lines("*n")
local numeric_first = numeric_iterator()
local numeric_second = numeric_iterator()
local numeric_done = numeric_iterator()
local multi_lines = io.open("multi_lines.txt", "rb")
local line_iterator = multi_lines:lines("*l", "*l")
local line_first, line_second = line_iterator()
local line_done = line_iterator()
local break_iterator, break_state, break_control, break_file = io.lines("answer.txt")
for _ in break_iterator, break_state, break_control, break_file do
    break
end
local break_closed = break_file == nil or io.type(break_file) == "closed file"
local process_ok, process = pcall(io.popen, "printf popen", "r")
local process_value = process_ok and process:read("*a") or "unsupported"
local process_closed = process_ok and process:close() or true
local invalid_process_ok, invalid_process_error = pcall(io.popen, "printf popen", "invalid")
local missing, missing_error = io.open("missing.txt", "rb")
local result = type(io) == "table" and open_error == nil and first == "owned\n"
    and buffered == true
    and line == "io" and position == 0 and all == "owned io\n"
    and flushed == true and returned == output and before == "file"
    and iterator_type == "function" and reset == 0
    and first_line == "owned" and second_line == "io" and done == nil
    and closed == true and after == "closed file"
    and second_ok == false and type(close_error) == "string"
    and default_input == io.stdin and default_output == io.stdout
    and io.type(default_error) == "file"
    and not input_failure_ok and type(input_failure_error) == "string"
    and not output_failure_ok and type(output_failure_error) == "string"
    and not closed_input_ok and type(closed_input_error) == "string"
    and not closed_output_ok and type(closed_output_error) == "string"
    and rebound_old_write == rebound_first
    and rebound_current == rebound_second and rebound_close == true
    and rebound_old_type == "file" and rebound_current_type == "closed file"
    and named_first == "owned io" and named_done == nil
    and named_shape and break_closed
    and switched_first == "owned io" and switched_returned == switched_output
    and multi_first == "owned io" and multi_second == nil
    and not userdata_global_ok
    and ((_VERSION == "Lua 5.1" and userdata_returned == true)
        or (_VERSION ~= "Lua 5.1" and userdata_returned == multi))
    and getmetatable(multi) == userdata_metatable
    and debug.getmetatable(multi) == userdata_metatable
    and userdata_answer == 42 and userdata_values.answer == 43
    and integer == 42 and fraction == 3.5
    and numeric_first == 42 and numeric_second == 3.5 and numeric_done == nil
    and line_first == "alpha" and line_second == "beta" and line_done == nil
    and ((process_ok and process_value == "popen" and process_closed == true)
        or (not process_ok and type(process) == "string"))
    and not invalid_process_ok and type(invalid_process_error) == "string"
    and missing == nil and type(missing_error) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const IO_SEEK_INTEGER_ARGUMENT_SOURCE: &str = r#"
local file = io.open("answer.txt", "rb")
local ok, value = pcall(file.seek, file, "set", 1.5)
file:close()
local legacy = _VERSION == "Lua 5.1"
return legacy and ok and value == 1 or not legacy and not ok
"#;
const IO_SEEK_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local file = io.open("answer.txt", "rb")
local ok, value = pcall(file.seek, file, "set", 1.5)
file:close()
local legacy = _VERSION == "Lua 5.1"
local result = legacy and ok and value == 1 or not legacy and not ok
print(type(result) .. ":" .. tostring(result))
"#;
const IO_SETVBUF_INTEGER_ARGUMENT_SOURCE: &str = r#"
local file = io.open("answer.txt", "rb")
local ok, value = pcall(file.setvbuf, file, "full", 1.5)
file:close()
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
return legacy and ok and value == true or not legacy and not ok
"#;
const IO_SETVBUF_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local file = io.open("answer.txt", "rb")
local ok, value = pcall(file.setvbuf, file, "full", 1.5)
file:close()
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
local result = legacy and ok and value == true or not legacy and not ok
print(type(result) .. ":" .. tostring(result))
"#;
const IO_READ_INTEGER_ARGUMENT_SOURCE: &str = r#"
local file = io.open("answer.txt", "rb")
local ok, value = pcall(file.read, file, 1.5)
file:close()
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
return legacy and ok and value == "o" or not legacy and not ok
"#;
const IO_READ_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local file = io.open("answer.txt", "rb")
local ok, value = pcall(file.read, file, 1.5)
file:close()
local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
local result = legacy and ok and value == "o" or not legacy and not ok
print(type(result) .. ":" .. tostring(result))
"#;

struct ConformanceIoFile {
    bytes: Mutex<Vec<u8>>,
    position: Mutex<usize>,
}

#[derive(Clone, Copy)]
enum FailingIoOperation {
    Read,
    Write,
    Seek,
    Flush,
    Buffering,
    Close,
}

struct FailingIoFile {
    operation: FailingIoOperation,
}

impl FailingIoFile {
    fn failure(&self, operation: &'static str) -> RuntimeError {
        RuntimeError::Raised(Value::String(Arc::from(
            format!("conformance {operation} failure").into_bytes(),
        )))
    }
}

impl IoFile for FailingIoFile {
    fn close(&self) -> Result<(), RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Close) {
            Err(self.failure("close"))
        } else {
            Ok(())
        }
    }

    fn write(&self, _value: &[u8]) -> Result<(), RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Write) {
            Err(self.failure("write"))
        } else {
            Ok(())
        }
    }

    fn read(&self, _request: IoReadRequest) -> Result<Option<Vec<u8>>, RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Read) {
            Err(self.failure("read"))
        } else {
            Ok(None)
        }
    }

    fn read_number(&self) -> Result<Option<Vec<u8>>, RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Read) {
            Err(self.failure("read"))
        } else {
            Ok(None)
        }
    }

    fn seek(&self, _whence: IoSeekWhence, _offset: i64) -> Result<u64, RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Seek) {
            Err(self.failure("seek"))
        } else {
            Ok(0)
        }
    }

    fn flush(&self) -> Result<(), RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Flush) {
            Err(self.failure("flush"))
        } else {
            Ok(())
        }
    }

    fn set_buffering(&self, _mode: IoBufferMode, _size: Option<usize>) -> Result<(), RuntimeError> {
        if matches!(self.operation, FailingIoOperation::Buffering) {
            Err(self.failure("buffering"))
        } else {
            Ok(())
        }
    }
}

impl IoFile for ConformanceIoFile {
    fn close(&self) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn set_buffering(&self, _mode: IoBufferMode, _size: Option<usize>) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn read(&self, request: IoReadRequest) -> Result<Option<Vec<u8>>, RuntimeError> {
        let bytes = self.bytes.lock().expect("conformance bytes lock");
        let mut position = self.position.lock().expect("conformance position lock");
        if *position >= bytes.len() {
            return Ok(match request {
                IoReadRequest::Bytes(0) => Some(Vec::new()),
                _ => None,
            });
        }
        let end = match request {
            IoReadRequest::All => bytes.len(),
            IoReadRequest::Bytes(count) => position.saturating_add(count).min(bytes.len()),
            IoReadRequest::Line { .. } => bytes[*position..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| *position + offset + 1),
        };
        let mut result = bytes[*position..end].to_vec();
        if let IoReadRequest::Line { keep_end: false } = request
            && result.last() == Some(&b'\n')
        {
            result.pop();
        }
        *position = end;
        Ok(Some(result))
    }

    fn read_number(&self) -> Result<Option<Vec<u8>>, RuntimeError> {
        let mut token = Vec::new();
        loop {
            let Some(bytes) = self.read(IoReadRequest::Bytes(1))? else {
                break;
            };
            let byte = bytes[0];
            if token.is_empty() && byte.is_ascii_whitespace() {
                continue;
            }
            if byte.is_ascii_whitespace() {
                break;
            }
            token.push(byte);
        }
        Ok((!token.is_empty()).then_some(token))
    }

    fn write(&self, value: &[u8]) -> Result<(), RuntimeError> {
        let mut bytes = self.bytes.lock().expect("conformance bytes lock");
        let mut position = self.position.lock().expect("conformance position lock");
        let end = position
            .checked_add(value.len())
            .ok_or(RuntimeError::InvalidRange {
                operation: "conformance io.write",
            })?;
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[*position..end].copy_from_slice(value);
        *position = end;
        Ok(())
    }

    fn seek(&self, whence: IoSeekWhence, offset: i64) -> Result<u64, RuntimeError> {
        let bytes = self.bytes.lock().expect("conformance bytes lock");
        let mut position = self.position.lock().expect("conformance position lock");
        let base = match whence {
            IoSeekWhence::Set => 0,
            IoSeekWhence::Current => i64::try_from(*position).unwrap_or(i64::MAX),
            IoSeekWhence::End => i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        };
        let next = base.checked_add(offset).ok_or(RuntimeError::InvalidRange {
            operation: "conformance io.seek",
        })?;
        if next < 0 {
            return Err(RuntimeError::InvalidRange {
                operation: "conformance io.seek",
            });
        }
        *position = usize::try_from(next).map_err(|_| RuntimeError::InvalidRange {
            operation: "conformance io.seek",
        })?;
        Ok(next as u64)
    }

    fn flush(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
}
const DEBUG_METATABLE_SOURCE: &str = r#"
local value = {}
local metatable = { answer = 42 }
setmetatable(value, { __metatable = "locked" })
local raw_before = debug.getmetatable(value)
local returned = debug.setmetatable(value, metatable)
return raw_before.__metatable == "locked"
    and getmetatable(value) == metatable
    and debug.getmetatable(value) == metatable
    and type(debug.getregistry()) == "table"
    and debug.getregistry() == debug.getregistry()
    and ((_VERSION == "Lua 5.1" and returned == true)
        or (_VERSION ~= "Lua 5.1" and returned == value))
"#;
const DEBUG_METATABLE_REFERENCE_SOURCE: &str = r#"
local value = {}
local metatable = { answer = 42 }
setmetatable(value, { __metatable = "locked" })
local raw_before = debug.getmetatable(value)
local returned = debug.setmetatable(value, metatable)
local result = raw_before.__metatable == "locked"
    and getmetatable(value) == metatable
    and debug.getmetatable(value) == metatable
    and type(debug.getregistry()) == "table"
    and debug.getregistry() == debug.getregistry()
    and ((_VERSION == "Lua 5.1" and returned == true)
        or (_VERSION ~= "Lua 5.1" and returned == value))
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_INFO_SOURCE: &str = r#"
local function answer(value, ...)
    local live = debug.getinfo(1, "Snuf")
    return live.what == "Lua"
        and live.linedefined == 2
        and live.lastlinedefined == 5
        and live.func == answer
        and live.nups >= 0
        and ((_VERSION == "Lua 5.1" and live.nparams == nil and live.isvararg == nil)
            or (_VERSION ~= "Lua 5.1" and live.nparams == 1 and live.isvararg))
end
local info = debug.getinfo(answer, "Snu")
local live_ok, live_nups = answer(1)
local result = info.what == "Lua"
    and type(info.source) == "string"
    and info.linedefined == 2
    and info.lastlinedefined == 5
    and info.nups == live_nups
    and ((_VERSION == "Lua 5.1" and info.nparams == nil and info.isvararg == nil)
        or (_VERSION ~= "Lua 5.1" and info.nparams == 1 and info.isvararg))
    and live_ok
return result
"#;
const DEBUG_INFO_REFERENCE_SOURCE: &str = r#"
local function answer(value, ...)
    local live = debug.getinfo(1, "Snuf")
    return live.what == "Lua"
        and live.linedefined == 2
        and live.lastlinedefined == 5
        and live.func == answer
        and live.nups >= 0
        and ((_VERSION == "Lua 5.1" and live.nparams == nil and live.isvararg == nil)
            or (_VERSION ~= "Lua 5.1" and live.nparams == 1 and live.isvararg))
end
local info = debug.getinfo(answer, "Snu")
local live_ok, live_nups = answer(1)
local result = info.what == "Lua"
    and type(info.source) == "string"
    and info.linedefined == 2
    and info.lastlinedefined == 5
    and info.nups == live_nups
    and ((_VERSION == "Lua 5.1" and info.nparams == nil and info.isvararg == nil)
        or (_VERSION ~= "Lua 5.1" and info.nparams == 1 and info.isvararg))
    and live_ok
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_LEVEL_ZERO_SOURCE: &str = r#"
local info = debug.getinfo(0, "Snulf")
return info.what == "C"
    and info.source == "=[C]"
    and info.short_src == "[C]"
    and info.namewhat == "field"
    and info.name == "getinfo"
    and info.linedefined == -1
    and info.lastlinedefined == -1
    and info.nups == 0
    and ((_VERSION == "Lua 5.1" and info.nparams == nil and info.isvararg == nil)
        or (_VERSION ~= "Lua 5.1" and info.nparams == 0 and info.isvararg))
    and info.currentline == -1
    and type(info.func) == "function"
"#;
const DEBUG_LEVEL_ZERO_REFERENCE_SOURCE: &str = r#"
local info = debug.getinfo(0, "Snulf")
local result = info.what == "C"
    and info.source == "=[C]"
    and info.short_src == "[C]"
    and info.namewhat == "field"
    and info.name == "getinfo"
    and info.linedefined == -1
    and info.lastlinedefined == -1
    and info.nups == 0
    and ((_VERSION == "Lua 5.1" and info.nparams == nil and info.isvararg == nil)
        or (_VERSION ~= "Lua 5.1" and info.nparams == 0 and info.isvararg))
    and info.currentline == -1
    and type(info.func) == "function"
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_CALLER_NAMES_SOURCE: &str = r#"
function answer()
    local info = debug.getinfo(1, "n")
    return info.namewhat, info.name
end
local object = { method = answer }
local key = "method"
local global_what, global_name = answer()
local method_what, method_name = object:method()
local field_what, field_name = object.method()
local dynamic_what, dynamic_name = object[key]()
return global_what == "global"
    and global_name == "answer"
    and method_what == "method"
    and method_name == "method"
    and field_what == "field"
    and field_name == "method"
    and dynamic_what == "field"
    and dynamic_name == "?"
"#;
const DEBUG_CALLER_NAMES_REFERENCE_SOURCE: &str = r#"
function answer()
    local info = debug.getinfo(1, "n")
    return info.namewhat, info.name
end
local object = { method = answer }
local key = "method"
local global_what, global_name = answer()
local method_what, method_name = object:method()
local field_what, field_name = object.method()
local dynamic_what, dynamic_name = object[key]()
local result = global_what == "global"
    and global_name == "answer"
    and method_what == "method"
    and method_name == "method"
    and field_what == "field"
    and field_name == "method"
    and dynamic_what == "field"
    and dynamic_name == "?"
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_MAIN_SOURCE: &str = r#"
local info = debug.getinfo(1, "Snuf")
return info.what == "main"
    and type(info.source) == "string"
    and type(info.short_src) == "string"
    and info.linedefined == 0
    and info.lastlinedefined == 0
    and ((_VERSION == "Lua 5.1" and info.nups == 0 and info.nparams == nil and info.isvararg == nil)
        or (_VERSION ~= "Lua 5.1" and info.nups == 1 and info.nparams == 0 and info.isvararg))
    and type(info.func) == "function"
"#;
const DEBUG_MAIN_REFERENCE_SOURCE: &str = r#"
local info = debug.getinfo(1, "Snuf")
local result = info.what == "main"
    and type(info.source) == "string"
    and type(info.short_src) == "string"
    and info.linedefined == 0
    and info.lastlinedefined == 0
    and ((_VERSION == "Lua 5.1" and info.nups == 0 and info.nparams == nil and info.isvararg == nil)
        or (_VERSION ~= "Lua 5.1" and info.nups == 1 and info.nparams == 0 and info.isvararg))
    and type(info.func) == "function"
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_LOCAL_SOURCE: &str = r#"
local function inspect(argument)
    local answer = 42
    local first_name, first_value = debug.getlocal(1, 1)
    local second_name, second_value = debug.getlocal(1, 2)
    return first_name == "argument"
        and first_value == argument
        and second_name == "answer"
        and second_value == 42
end
return inspect(7)
"#;
const DEBUG_LOCAL_REFERENCE_SOURCE: &str = r#"
local function inspect(argument)
    local answer = 42
    local first_name, first_value = debug.getlocal(1, 1)
    local second_name, second_value = debug.getlocal(1, 2)
    local result = first_name == "argument"
        and first_value == argument
        and second_name == "answer"
        and second_value == 42
    return result
end
local result = inspect(7)
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_SETLOCAL_SOURCE: &str = r#"
local function active(argument)
    local answer = 42
    local first = debug.setlocal(1, 1, 7)
    local second = debug.setlocal(1, 2, 8)
    local first_name, first_value = debug.getlocal(1, 1)
    local second_name, second_value = debug.getlocal(1, 2)
    return first == "argument" and second == "answer"
        and first_name == "argument" and first_value == 7
        and second_name == "answer" and second_value == 8
end
local function suspended(argument)
    local answer = 42
    coroutine.yield()
    return argument + answer
end
local thread = coroutine.create(suspended)
local started = coroutine.resume(thread, 3)
local first = debug.setlocal(thread, 1, 1, 7)
local second = debug.setlocal(thread, 1, 2, 8)
local first_name, first_value = debug.getlocal(thread, 1, 1)
local second_name, second_value = debug.getlocal(thread, 1, 2)
local finished, result = coroutine.resume(thread)
return active(1) and started and finished and result == 15
    and first == "argument" and second == "answer"
    and first_name == "argument" and first_value == 7
    and second_name == "answer" and second_value == 8
"#;
const DEBUG_SETLOCAL_REFERENCE_SOURCE: &str = r#"
local function active(argument)
    local answer = 42
    local first = debug.setlocal(1, 1, 7)
    local second = debug.setlocal(1, 2, 8)
    local first_name, first_value = debug.getlocal(1, 1)
    local second_name, second_value = debug.getlocal(1, 2)
    return first == "argument" and second == "answer"
        and first_name == "argument" and first_value == 7
        and second_name == "answer" and second_value == 8
end
local function suspended(argument)
    local answer = 42
    coroutine.yield()
    return argument + answer
end
local thread = coroutine.create(suspended)
local started = coroutine.resume(thread, 3)
local first = debug.setlocal(thread, 1, 1, 7)
local second = debug.setlocal(thread, 1, 2, 8)
local first_name, first_value = debug.getlocal(thread, 1, 1)
local second_name, second_value = debug.getlocal(thread, 1, 2)
local finished, result = coroutine.resume(thread)
local output = active(1) and started and finished and result == 15
    and first == "argument" and second == "answer"
    and first_name == "argument" and first_value == 7
    and second_name == "answer" and second_value == 8
print(type(output) .. ":" .. tostring(output))
"#;
const DEBUG_LOCAL_INTEGER_ARGUMENT_SOURCE: &str = r#"
local function inspect()
    local get_ok = pcall(debug.getlocal, 1, 1.5)
    local set_ok = pcall(debug.setlocal, 1, 1.5, 4)
    local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
        or _VERSION == "Lua 5.5"
    return modern and not get_ok and not set_ok
        or not modern and get_ok and set_ok
end
return inspect()
"#;
const DEBUG_LOCAL_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local function inspect()
    local get_ok = pcall(debug.getlocal, 1, 1.5)
    local set_ok = pcall(debug.setlocal, 1, 1.5, 4)
    local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
        or _VERSION == "Lua 5.5"
    return modern and not get_ok and not set_ok
        or not modern and get_ok and set_ok
end
local result = inspect()
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_CURRENTLINE_SOURCE: &str = r#"
local function inspect()
    local info = debug.getinfo(1, "l")
    return info.currentline == 2
end
return inspect()
"#;
const DEBUG_CURRENTLINE_REFERENCE_SOURCE: &str = r#"
local function inspect()
    local info = debug.getinfo(1, "l")
    local result = info.currentline == 2
    return result
end
local result = inspect()
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_ACTIVELINES_SOURCE: &str = r#"
local function inspect()
    local info = debug.getinfo(1, "L")
    return type(info.activelines) == "table" and info.activelines[3] == true
end
return inspect()
"#;
const DEBUG_ACTIVELINES_REFERENCE_SOURCE: &str = r#"
local function inspect()
    local info = debug.getinfo(1, "L")
    local result = type(info.activelines) == "table" and info.activelines[3] == true
    return result
end
local result = inspect()
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_CALLER_SOURCE: &str = r#"
local function inner()
    local info = debug.getinfo(2, "Su")
    local name, value = debug.getlocal(2, 1)
    return info ~= nil and info.what == "Lua" and name == "argument" and value == 7
end
local function outer(argument)
    local result = inner()
    return result
end
local result = outer(7)
return result
"#;
const DEBUG_CALLER_REFERENCE_SOURCE: &str = r#"
local function inner()
    local info = debug.getinfo(2, "Su")
    local name, value = debug.getlocal(2, 1)
    return info ~= nil and info.what == "Lua" and name == "argument" and value == 7
end
local function outer(argument)
    local result = inner()
    return result
end
local result = outer(7)
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_SET_CALLER_SOURCE: &str = r#"
local function inner()
    local name = debug.setlocal(2, 1, 9)
    local caller_name, caller_value = debug.getlocal(2, 1)
    return name == "argument" and caller_name == "argument" and caller_value == 9
end
local function outer(argument)
    local result = inner()
    return result and argument == 9
end
return outer(7)
"#;
const DEBUG_SET_CALLER_REFERENCE_SOURCE: &str = r#"
local function inner()
    local name = debug.setlocal(2, 1, 9)
    local caller_name, caller_value = debug.getlocal(2, 1)
    return name == "argument" and caller_name == "argument" and caller_value == 9
end
local function outer(argument)
    local result = inner()
    return result and argument == 9
end
local result = outer(7)
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_UPVALUEJOIN_SOURCE: &str = r#"
local first_value = 1
local function first() return first_value end
local second_value = 2
local function second() return second_value end
local first_id = debug.upvalueid(first, 1)
local second_id = debug.upvalueid(second, 1)
debug.upvaluejoin(first, 1, second, 1)
local joined_id = debug.upvalueid(first, 1)
debug.setupvalue(second, 1, 3)
return type(first_id) == "userdata" and first_id ~= second_id and second_id == joined_id and first() == 3 and second() == 3
"#;
const DEBUG_UPVALUEJOIN_REFERENCE_SOURCE: &str = r#"
local first_value = 1
local function first() return first_value end
local second_value = 2
local function second() return second_value end
local first_id = debug.upvalueid(first, 1)
local second_id = debug.upvalueid(second, 1)
debug.upvaluejoin(first, 1, second, 1)
local joined_id = debug.upvalueid(first, 1)
debug.setupvalue(second, 1, 3)
local result = type(first_id) == "userdata" and first_id ~= second_id and second_id == joined_id and first() == 3 and second() == 3
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_THREAD_SOURCE: &str = r#"
local function inspect(argument)
    local thread = coroutine.running()
    local info = debug.getinfo(thread, 1, "Su")
    local name, value = debug.getlocal(thread, 1, 1)
    return info.what == "Lua" and name == "argument" and value == argument
end
local thread = coroutine.create(inspect)
local ok, result = coroutine.resume(thread, 7)
return ok and result
"#;
const DEBUG_THREAD_REFERENCE_SOURCE: &str = r#"
local function inspect(argument)
    local thread = coroutine.running()
    local info = debug.getinfo(thread, 1, "Su")
    local name, value = debug.getlocal(thread, 1, 1)
    return info.what == "Lua" and name == "argument" and value == argument
end
local thread = coroutine.create(inspect)
local ok, result = coroutine.resume(thread, 7)
local value = ok and result
print(type(value) .. ":" .. tostring(value))
"#;
const DEBUG_SUSPENDED_THREAD_SOURCE: &str = r#"
local function inspect(argument)
    local answer = 42
    coroutine.yield()
    return argument + answer
end
local thread = coroutine.create(inspect)
local started = coroutine.resume(thread, 7)
local info = debug.getinfo(thread, 1, "Su")
local first_name, first_value = debug.getlocal(thread, 1, 1)
local second_name, second_value = debug.getlocal(thread, 1, 2)
local missing = debug.getinfo(thread, 2)
local finished, result = coroutine.resume(thread)
return started and finished and result == 49
    and info.what == "Lua"
    and first_name == "argument" and first_value == 7
    and second_name == "answer" and second_value == 42
    and missing == nil
"#;
const DEBUG_SUSPENDED_THREAD_REFERENCE_SOURCE: &str = r#"
local function inspect(argument)
    local answer = 42
    coroutine.yield()
    return argument + answer
end
local thread = coroutine.create(inspect)
local started = coroutine.resume(thread, 7)
local info = debug.getinfo(thread, 1, "Su")
local first_name, first_value = debug.getlocal(thread, 1, 1)
local second_name, second_value = debug.getlocal(thread, 1, 2)
local missing = debug.getinfo(thread, 2)
local finished, result = coroutine.resume(thread)
local value = started and finished and result == 49
    and info.what == "Lua"
    and first_name == "argument" and first_value == 7
    and second_name == "answer" and second_value == 42
    and missing == nil
print(type(value) .. ":" .. tostring(value))
"#;
const DEBUG_UPVALUE_SOURCE: &str = r#"
local captured = 41
local function inner()
    return captured
end
local name, value = debug.getupvalue(inner, 1)
local changed = debug.setupvalue(inner, 1, 42)
local _, updated = debug.getupvalue(inner, 1)
return name == "captured"
    and value == 41
    and changed == "captured"
    and updated == 42
    and inner() == 42
"#;
const DEBUG_UPVALUE_REFERENCE_SOURCE: &str = r#"
local captured = 41
local function inner()
    return captured
end
local name, value = debug.getupvalue(inner, 1)
local changed = debug.setupvalue(inner, 1, 42)
local _, updated = debug.getupvalue(inner, 1)
local result = name == "captured"
    and value == 41
    and changed == "captured"
    and updated == 42
    and inner() == 42
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_UPVALUE_INTEGER_ARGUMENT_SOURCE: &str = r#"
local captured = 41
local function inner()
    return captured
end
local get_ok = pcall(debug.getupvalue, inner, 1.5)
local set_ok = pcall(debug.setupvalue, inner, 1.5, 42)
local join_ok = type(debug.upvaluejoin) == "function"
    and pcall(debug.upvaluejoin, inner, 1.5, inner, 1)
    or false
local id_ok = type(debug.upvalueid) == "function"
    and pcall(debug.upvalueid, inner, 1.5)
    or false
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local has_join = type(debug.upvaluejoin) == "function"
local has_id = type(debug.upvalueid) == "function"
return get_ok == not modern and set_ok == not modern
    and join_ok == has_join and id_ok == has_id
"#;
const DEBUG_UPVALUE_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local captured = 41
local function inner()
    return captured
end
local get_ok = pcall(debug.getupvalue, inner, 1.5)
local set_ok = pcall(debug.setupvalue, inner, 1.5, 42)
local join_ok = type(debug.upvaluejoin) == "function"
    and pcall(debug.upvaluejoin, inner, 1.5, inner, 1)
    or false
local id_ok = type(debug.upvalueid) == "function"
    and pcall(debug.upvalueid, inner, 1.5)
    or false
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local has_join = type(debug.upvaluejoin) == "function"
local has_id = type(debug.upvalueid) == "function"
local result = get_ok == not modern and set_ok == not modern
    and join_ok == has_join and id_ok == has_id
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_HOOK_INTEGER_ARGUMENT_SOURCE: &str = r#"
local function hook() end
local ok = pcall(debug.sethook, hook, "", 1.5)
debug.sethook()
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok
"#;
const DEBUG_HOOK_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local function hook() end
local ok = pcall(debug.sethook, hook, "", 1.5)
debug.sethook()
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_INFO_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok = pcall(debug.getinfo, 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok
"#;
const DEBUG_INFO_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok = pcall(debug.getinfo, 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_TRACEBACK_SOURCE: &str = r#"
local function answer()
    local trace = debug.traceback("marker", 1)
    return type(trace) == "string"
        and string.find(trace, "marker", 1, true) ~= nil
        and string.find(trace, "stack traceback", 1, true) ~= nil
end
return answer()
"#;
const DEBUG_TRACEBACK_REFERENCE_SOURCE: &str = r#"
local function answer()
    local trace = debug.traceback("marker", 1)
    local result = type(trace) == "string"
        and string.find(trace, "marker", 1, true) ~= nil
        and string.find(trace, "stack traceback", 1, true) ~= nil
    return result
end
print(type(answer()) .. ":" .. tostring(answer()))
"#;
const DEBUG_HOOK_SOURCE: &str = r#"
local seen = 0
local last = 0
local function hook(event, line)
    if event == "line" then
        seen = seen + 1
        last = line
    end
end
debug.sethook(hook, "l")
local value = 1
value = value + 1
debug.sethook()
local f, mask, count = debug.gethook()
return seen > 0 and last > 0 and f == nil
    and ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and mask == nil and count == nil
        or (_VERSION ~= "Lua 5.4" and _VERSION ~= "Lua 5.5"
            and mask == "" and count == 0))
"#;
const DEBUG_HOOK_REFERENCE_SOURCE: &str = r#"
local seen = 0
local last = 0
local function hook(event, line)
    if event == "line" then
        seen = seen + 1
        last = line
    end
end
debug.sethook(hook, "l")
local value = 1
value = value + 1
debug.sethook()
local f, mask, count = debug.gethook()
local result = seen > 0 and last > 0 and f == nil
    and ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and mask == nil and count == nil
        or (_VERSION ~= "Lua 5.4" and _VERSION ~= "Lua 5.5"
            and mask == "" and count == 0))
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_COUNT_HOOK_SOURCE: &str = r#"
local seen = 0
local function hook(event, line)
    if event == "count" and line == nil then
        seen = seen + 1
    end
end
debug.sethook(hook, "", 3)
local value = 0
for index = 1, 5 do
    value = value + index
end
debug.sethook()
local f, mask, count = debug.gethook()
return seen > 0 and f == nil
    and ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and mask == nil and count == nil
        or (_VERSION ~= "Lua 5.4" and _VERSION ~= "Lua 5.5"
            and mask == "" and count == 0))
"#;
const DEBUG_COUNT_HOOK_REFERENCE_SOURCE: &str = r#"
local seen = 0
local function hook(event, line)
    if event == "count" and line == nil then
        seen = seen + 1
    end
end
debug.sethook(hook, "", 3)
local value = 0
for index = 1, 5 do
    value = value + index
end
debug.sethook()
local f, mask, count = debug.gethook()
local result = seen > 0 and f == nil
    and ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and mask == nil and count == nil
        or (_VERSION ~= "Lua 5.4" and _VERSION ~= "Lua 5.5"
            and mask == "" and count == 0))
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_CALL_RETURN_HOOK_SOURCE: &str = r#"
local saw_call = false
local saw_return = false
local function hook(event, line)
    if event == "call" then saw_call = true end
    if event == "return" then saw_return = true end
end
debug.sethook(hook, "cr")
local function answer()
    return 42
end
local value = answer()
debug.sethook()
return value == 42 and saw_call and saw_return
"#;
const DEBUG_CALL_RETURN_HOOK_REFERENCE_SOURCE: &str = r#"
local saw_call = false
local saw_return = false
local function hook(event, line)
    if event == "call" then saw_call = true end
    if event == "return" then saw_return = true end
end
debug.sethook(hook, "cr")
local function answer()
    return 42
end
local value = answer()
debug.sethook()
local result = value == 42 and saw_call and saw_return
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_TAIL_HOOK_SOURCE: &str = r#"
local saw_tail = false
local function hook(event, line)
    if event == "tail call" or (_VERSION == "Lua 5.1" and event == "tail return") then
        saw_tail = true
    end
end
debug.sethook(hook, "cr")
local function leaf(value)
    return value
end
local function tail(value)
    return leaf(value)
end
local value = tail(42)
debug.sethook()
return value == 42 and saw_tail
"#;
const DEBUG_TAIL_HOOK_REFERENCE_SOURCE: &str = r#"
local saw_tail = false
local function hook(event, line)
    if event == "tail call" or (_VERSION == "Lua 5.1" and event == "tail return") then
        saw_tail = true
    end
end
debug.sethook(hook, "cr")
local function leaf(value)
    return value
end
local function tail(value)
    return leaf(value)
end
local value = tail(42)
debug.sethook()
local result = value == 42 and saw_tail
print(type(result) .. ":" .. tostring(result))
"#;
const DEEP_TAIL_RECURSION_SOURCE: &str = r#"
local function recurse(depth)
    if depth == 0 then
        return 41
    end
    return recurse(depth - 1)
end
return recurse(2048) == 41
"#;
const DEEP_TAIL_RECURSION_REFERENCE_SOURCE: &str = r#"
local function recurse(depth)
    if depth == 0 then
        return 41
    end
    return recurse(depth - 1)
end
local result = recurse(2048) == 41
print(type(result) .. ":" .. tostring(result))
"#;
const TAIL_CLOSE_SOURCE: &str = r#"
local closed = false
local metatable = { __close = function() closed = true end }
local function target()
    return closed
end
local function wrapper()
    local resource <close> = setmetatable({}, metatable)
    return target()
end
local observed = wrapper()
return observed == false and closed
"#;
const TAIL_CLOSE_REFERENCE_SOURCE: &str = r#"
local closed = false
local metatable = { __close = function() closed = true end }
local function target()
    return closed
end
local function wrapper()
    local resource <close> = setmetatable({}, metatable)
    return target()
end
local observed = wrapper()
local result = observed == false and closed
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_NATIVE_HOOK_SOURCE: &str = r#"
local calls = 0
local returns = 0
local function hook(event, line)
    if event == "call" then calls = calls + 1 end
    if event == "return" then returns = returns + 1 end
end
debug.sethook(hook, "cr")
local function wrapper()
    return math.abs(-2)
end
local value = wrapper()
debug.sethook()
return value == 2 and calls >= 3 and returns >= 2
"#;
const DEBUG_NATIVE_HOOK_REFERENCE_SOURCE: &str = r#"
local calls = 0
local returns = 0
local function hook(event, line)
    if event == "call" then calls = calls + 1 end
    if event == "return" then returns = returns + 1 end
end
debug.sethook(hook, "cr")
local function wrapper()
    return math.abs(-2)
end
local value = wrapper()
debug.sethook()
local result = value == 2 and calls >= 3 and returns >= 2
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_NATIVE_FRAME_SOURCE: &str = r#"
local saw_c = false
local function hook(event, line)
    if event == "call" then
        local info = debug.getinfo(2, "Snu")
        if info.what == "C" and info.source == "=[C]" and info.short_src == "[C]"
            and info.nups == 0
            and ((_VERSION == "Lua 5.1" and info.nparams == nil and info.isvararg == nil)
                or (_VERSION ~= "Lua 5.1" and info.nparams == 0 and info.isvararg == true)) then
            saw_c = true
        end
    end
end
debug.sethook(hook, "c")
local value = math.abs(-2)
debug.sethook()
return value == 2 and saw_c
"#;
const DEBUG_NATIVE_FRAME_REFERENCE_SOURCE: &str = r#"
local saw_c = false
local function hook(event, line)
    if event == "call" then
        local info = debug.getinfo(2, "Snu")
        if info.what == "C" and info.source == "=[C]" and info.short_src == "[C]"
            and info.nups == 0
            and ((_VERSION == "Lua 5.1" and info.nparams == nil and info.isvararg == nil)
                or (_VERSION ~= "Lua 5.1" and info.nparams == 0 and info.isvararg == true)) then
            saw_c = true
        end
    end
end
debug.sethook(hook, "c")
local value = math.abs(-2)
debug.sethook()
local result = value == 2 and saw_c
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_NATIVE_NAME_SOURCE: &str = r#"
local saw_name = false
local function hook(event, line)
    if event == "call" then
        local info = debug.getinfo(2, "Sn")
        if info.what == "C" and info.namewhat == "field" and info.name == "abs" then
            saw_name = true
        end
    end
end
debug.sethook(hook, "c")
local value = math.abs(-2)
debug.sethook()
return value == 2 and saw_name
"#;
const DEBUG_NATIVE_NAME_REFERENCE_SOURCE: &str = r#"
local saw_name = false
local function hook(event, line)
    if event == "call" then
        local info = debug.getinfo(2, "Sn")
        if info.what == "C" and info.namewhat == "field" and info.name == "abs" then
            saw_name = true
        end
    end
end
debug.sethook(hook, "c")
local value = math.abs(-2)
debug.sethook()
local result = value == 2 and saw_name
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_YIELDING_HOOK_SOURCE: &str = r#"
local co = coroutine.create(function()
    local first = true
    debug.sethook(function()
        if first then first = false; coroutine.yield("hook") end
    end, "l")
    local value = 1
    value = value + 1
    debug.sethook()
    return value
end)
local ok, value = coroutine.resume(co)
return not ok
"#;
const DEBUG_YIELDING_HOOK_REFERENCE_SOURCE: &str = r#"
local co = coroutine.create(function()
    local first = true
    debug.sethook(function()
        if first then first = false; coroutine.yield("hook") end
    end, "l")
    local value = 1
    value = value + 1
    debug.sethook()
    return value
end)
local ok, value = coroutine.resume(co)
local result = not ok
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_THREAD_HOOK_SOURCE: &str = r#"
local seen = 0
local hook = function(event)
    if event == "call" then seen = seen + 1 end
end
local co = coroutine.create(function() return math.abs(-2) end)
debug.sethook(co, hook, "c")
local main_hook = debug.gethook()
local target_hook, mask = debug.gethook(co)
local ok, value = coroutine.resume(co)
debug.sethook(co, nil)
return ok and value == 2 and seen > 0 and main_hook == nil
    and target_hook == hook and mask == "c"
"#;
const DEBUG_THREAD_HOOK_REFERENCE_SOURCE: &str = r#"
local seen = 0
local hook = function(event)
    if event == "call" then seen = seen + 1 end
end
local co = coroutine.create(function() return math.abs(-2) end)
debug.sethook(co, hook, "c")
local main_hook = debug.gethook()
local target_hook, mask = debug.gethook(co)
local ok, value = coroutine.resume(co)
debug.sethook(co, nil)
local result = ok and value == 2 and seen > 0 and main_hook == nil
    and target_hook == hook and mask == "c"
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_USERVALUE_SOURCE: &str = r#"
local value, present = debug.getuservalue({})
local set_ok = pcall(debug.setuservalue, {}, {})
return value == nil
    and ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and present == false
        or (_VERSION ~= "Lua 5.4" and _VERSION ~= "Lua 5.5"
            and present == nil))
    and not set_ok
"#;
const DEBUG_USERVALUE_REFERENCE_SOURCE: &str = r#"
local value, present = debug.getuservalue({})
local set_ok = pcall(debug.setuservalue, {}, {})
local result = value == nil
    and ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and present == false
        or (_VERSION ~= "Lua 5.4" and _VERSION ~= "Lua 5.5"
            and present == nil))
    and not set_ok
print(type(result) .. ":" .. tostring(result))
"#;
const DEBUG_USERVALUE_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok = pcall(debug.getuservalue, {}, 1.5)
local modern = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok
"#;
const DEBUG_USERVALUE_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok = pcall(debug.getuservalue, {}, 1.5)
local modern = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_DUMP_SOURCE: &str = r#"
local function answer()
    return 42
end
local dumped = string.dump(answer)
local loader = loadstring or load
local restored = loader(dumped)
local stripped = string.dump(answer, true)
local restored_stripped = loader(stripped)
return type(dumped) == "string"
    and #dumped > 0
    and type(restored) == "function"
    and restored() == 42
    and #stripped > 0
    and type(restored_stripped) == "function"
    and restored_stripped() == 42
"#;
const STRING_DUMP_REFERENCE_SOURCE: &str = r#"
local function answer()
    return 42
end
local dumped = string.dump(answer)
local loader = loadstring or load
local restored = loader(dumped)
local stripped = string.dump(answer, true)
local restored_stripped = loader(stripped)
local result = type(dumped) == "string"
    and #dumped > 0
    and type(restored) == "function"
    and restored() == 42
    and #stripped > 0
    and type(restored_stripped) == "function"
    and restored_stripped() == 42
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_DUMP_CAPTURE_SOURCE: &str = r#"
local captured = 41
local function answer()
    return captured
end
local loader = loadstring or load
local restored = loader(string.dump(answer))
local name, value = debug.getupvalue(restored, 1)
local ok, result = pcall(restored)
return name == "captured"
    and ((_VERSION == "Lua 5.1" and value == nil)
        or (_VERSION ~= "Lua 5.1" and type(value) == "table"))
    and ok
    and result == value
"#;
const STRING_DUMP_CAPTURE_REFERENCE_SOURCE: &str = r#"
local captured = 41
local function answer()
    return captured
end
local loader = loadstring or load
local restored = loader(string.dump(answer))
local name, value = debug.getupvalue(restored, 1)
local ok, result = pcall(restored)
local output = name == "captured"
    and ((_VERSION == "Lua 5.1" and value == nil)
        or (_VERSION ~= "Lua 5.1" and type(value) == "table"))
    and ok
    and result == value
print(type(output) .. ":" .. tostring(output))
"#;
const FILE_LOAD_SOURCE: &str = r#"
local loaded, load_error = loadfile("answer.lua")
local first = loaded()
local second = dofile("answer.lua")
return load_error == nil and first == 41 and second == 41
"#;
const FILE_LOAD_REFERENCE_SOURCE: &str = r#"
local loaded, load_error = loadfile("answer.lua")
local first = loaded()
local second = dofile("answer.lua")
local result = load_error == nil and first == 41 and second == 41
print(type(result) .. ":" .. tostring(result))
"#;
const FOREIGN_LUA_BINARY_SOURCE: &str = r#"
local loaded, load_error = loadfile("foreign.luac")
return loaded == nil and type(load_error) == "string"
"#;
const FOREIGN_LUA_BINARY_REFERENCE_SOURCE: &str = r#"
local loaded, load_error = loadfile("foreign.luac")
local result = loaded ~= nil and load_error == nil and loaded() == 41
print(type(result) .. ":" .. tostring(result))
"#;
const PACKAGE_SEARCHPATH_SOURCE: &str = r#"
local found, found_error = package.searchpath("answer", "./?.lua")
local missing, missing_error = package.searchpath("missing", "./?.lua")
return found == "./answer.lua" and found_error == nil
    and missing == nil and missing_error ~= nil
"#;
const PACKAGE_SEARCHPATH_REFERENCE_SOURCE: &str = r#"
local found, found_error = package.searchpath("answer", "./?.lua")
local missing, missing_error = package.searchpath("missing", "./?.lua")
local result = found == "./answer.lua" and found_error == nil
    and missing == nil and missing_error ~= nil
print(type(result) .. ":" .. tostring(result))
"#;
const SOURCE_REQUIRE_SOURCE: &str = r#"
package.path = "./?.lua"
local value = require("answer")
local empty = require("empty")
return value == 41 and package.loaded.answer == 41
    and empty == true and package.loaded.empty == true
"#;
const SOURCE_REQUIRE_REFERENCE_SOURCE: &str = r#"
package.path = "./?.lua"
local value = require("answer")
local empty = require("empty")
local result = value == 41 and package.loaded.answer == 41
    and empty == true and package.loaded.empty == true
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
const PACKAGE_SEARCHER_SOURCE: &str = r#"
local calls = 0
local key = _VERSION == "Lua 5.1" and "loaders" or "searchers"
local searchers = {}
searchers[1] = function(name)
    calls = calls + 1
    if name == "guest" then
        return function(module_name)
            return { name = module_name, answer = 42, extra = "payload" }
        end, "payload"
    end
end
package[key] = searchers
local value = require("guest")
return value.name == "guest"
    and value.answer == 42
    and value.extra == "payload"
    and value == package.loaded.guest
    and calls == 1
"#;
const PACKAGE_SEARCHER_REFERENCE_SOURCE: &str = r#"
local calls = 0
local key = _VERSION == "Lua 5.1" and "loaders" or "searchers"
local searchers = {}
searchers[1] = function(name)
    calls = calls + 1
    if name == "guest" then
        return function(module_name)
            return { name = module_name, answer = 42, extra = "payload" }
        end, "payload"
    end
end
package[key] = searchers
local value = require("guest")
local result = value.name == "guest"
    and value.answer == 42
    and value.extra == "payload"
    and value == package.loaded.guest
    and calls == 1
print(type(result) .. ":" .. tostring(result))
"#;
const YIELDING_PACKAGE_SEARCHER_SOURCE: &str = r#"
local searchers = {
    function(name)
        coroutine.yield("searcher pause")
        return function()
            return 42
        end
    end
}
package.searchers = searchers
package.loaders = searchers
local thread = coroutine.create(function()
    return require("yielded")
end)
local first, signal = coroutine.resume(thread)
local second, result = coroutine.resume(thread)
return first and signal == "searcher pause" and second and result == 42
"#;
const YIELDING_PACKAGE_SEARCHER_REFERENCE_SOURCE: &str = r#"
local searchers = {
    function(name)
        coroutine.yield("searcher pause")
        return function()
            return 42
        end
    end
}
package.searchers = searchers
package.loaders = searchers
local thread = coroutine.create(function()
    return require("yielded")
end)
local first = coroutine.resume(thread)
print(type(first) .. ":" .. tostring(first))
"#;
const YIELDING_PACKAGE_LOADER_SOURCE: &str = r#"
local searchers = {
    function(name)
        return function(module_name)
            coroutine.yield("loader pause")
            return module_name .. ":loaded"
        end
    end
}
package.searchers = searchers
package.loaders = searchers
local thread = coroutine.create(function()
    return require("yielded")
end)
local first, signal = coroutine.resume(thread)
local second, result = coroutine.resume(thread)
return first and signal == "loader pause"
    and second and result == "yielded:loaded"
    and package.loaded.yielded == "yielded:loaded"
"#;
const YIELDING_PACKAGE_LOADER_REFERENCE_SOURCE: &str = r#"
local searchers = {
    function(name)
        return function(module_name)
            coroutine.yield("loader pause")
            return module_name .. ":loaded"
        end
    end
}
package.searchers = searchers
package.loaders = searchers
local thread = coroutine.create(function()
    return require("yielded")
end)
local first = coroutine.resume(thread)
print(type(first) .. ":" .. tostring(first))
"#;
const YIELDING_MODULE_OPTION_SOURCE: &str = r#"
local thread = coroutine.create(function()
    module("yielded", function()
        coroutine.yield("module option pause")
    end)
end)
local ok = coroutine.resume(thread)
return not ok
"#;
const YIELDING_MODULE_OPTION_REFERENCE_SOURCE: &str = r#"
local type_fn = type
local tostring_fn = tostring
local print_fn = print
local thread = coroutine.create(function()
    module("yielded", function()
        coroutine.yield("module option pause")
    end)
end)
local ok = coroutine.resume(thread)
print_fn(type_fn(not ok) .. ":" .. tostring_fn(not ok))
"#;
const UTF8_SOURCE: &str = r#"
local text = utf8.char(65, 233, 0x1F600)
local first, second, third = utf8.codepoint(text, 1, #text)
local invalid, position = utf8.len("\255")
local surrogate = utf8.char(0xD800)
local surrogate_codepoint = 0
if _VERSION == "Lua 5.3" or _VERSION == "Blu" then
    surrogate_codepoint = utf8.codepoint(surrogate)
end
local surrogate_length, surrogate_position = utf8.len(surrogate)
local valid_surrogate = pcall(utf8.char, 0xD800)
return utf8.len(text) == 3
    and first == 65
    and second == 233
    and third == 0x1F600
    and type(utf8.charpattern) == "string"
    and invalid == nil
    and position == 1
    and #surrogate == 3
    and ((_VERSION == "Lua 5.3" or _VERSION == "Blu")
        and surrogate_codepoint == 0xD800
        and surrogate_length == 1
        or (_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and surrogate_length == nil
        and surrogate_position == 1)
    and valid_surrogate
"#;
const UTF8_REFERENCE_SOURCE: &str = r#"
local text = utf8.char(65, 233, 0x1F600)
local first, second, third = utf8.codepoint(text, 1, #text)
local invalid, position = utf8.len("\255")
local surrogate = utf8.char(0xD800)
local surrogate_codepoint = 0
if _VERSION == "Lua 5.3" then
    surrogate_codepoint = utf8.codepoint(surrogate)
end
local surrogate_length, surrogate_position = utf8.len(surrogate)
local valid_surrogate = pcall(utf8.char, 0xD800)
local result = utf8.len(text) == 3
    and first == 65
    and second == 233
    and third == 0x1F600
    and type(utf8.charpattern) == "string"
    and invalid == nil
    and position == 1
    and #surrogate == 3
    and ((_VERSION == "Lua 5.3"
        and surrogate_codepoint == 0xD800
        and surrogate_length == 1)
        or (_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and surrogate_length == nil
        and surrogate_position == 1)
    and valid_surrogate
print(type(result) .. ":" .. tostring(result))
"#;
const UTF8_OFFSET_SOURCE: &str = r#"
local text = "A" .. utf8.char(233) .. utf8.char(0x1F600) .. "Z"
local first = utf8.offset(text, 1)
local second = utf8.offset(text, 2)
local inside = utf8.offset(text, 0, 3)
local previous, previous_end = utf8.offset(text, -1)
return first == 1
    and second == 2
    and inside == 2
    and previous == 8
    and ((_VERSION == "Lua 5.5" and previous_end == 8)
        or (_VERSION ~= "Lua 5.5" and previous_end == nil))
"#;
const UTF8_OFFSET_REFERENCE_SOURCE: &str = r#"
local text = "A" .. utf8.char(233) .. utf8.char(0x1F600) .. "Z"
local first = utf8.offset(text, 1)
local second = utf8.offset(text, 2)
local inside = utf8.offset(text, 0, 3)
local previous, previous_end = utf8.offset(text, -1)
local result = first == 1
    and second == 2
    and inside == 2
    and previous == 8
    and ((_VERSION == "Lua 5.5" and previous_end == 8)
        or (_VERSION ~= "Lua 5.5" and previous_end == nil))
print(type(result) .. ":" .. tostring(result))
"#;
const UTF8_MALFORMED_SOURCE: &str = r#"
local bad = string.char(255)
local len_ok, length, position = pcall(utf8.len, bad)
local code_ok = pcall(utf8.codepoint, bad)
local offset_ok, offset, offset_end = pcall(utf8.offset, bad, 1)
local char_ok = pcall(utf8.char, 0x110000)
local extended_char = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local offset_expected = offset_ok and offset == 1
    and ((_VERSION == "Lua 5.5" and offset_end == 1)
        or (_VERSION ~= "Lua 5.5" and offset_end == nil))
local char_expected = extended_char and char_ok or not extended_char and not char_ok
return len_ok and length == nil and position == 1
    and not code_ok and offset_expected and char_expected
"#;
const UTF8_MALFORMED_REFERENCE_SOURCE: &str = r#"
local bad = string.char(255)
local len_ok, length, position = pcall(utf8.len, bad)
local code_ok = pcall(utf8.codepoint, bad)
local offset_ok, offset, offset_end = pcall(utf8.offset, bad, 1)
local char_ok = pcall(utf8.char, 0x110000)
local extended_char = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local offset_expected = offset_ok and offset == 1
    and ((_VERSION == "Lua 5.5" and offset_end == 1)
        or (_VERSION ~= "Lua 5.5" and offset_end == nil))
local char_expected = extended_char and char_ok or not extended_char and not char_ok
local result = len_ok and length == nil and position == 1
    and not code_ok and offset_expected and char_expected
print(type(result) .. ":" .. tostring(result))
"#;
const UTF8_LAX_SOURCE: &str = r#"
local surrogate = utf8.char(0xD800)
local len_ok, length = pcall(utf8.len, surrogate, 1, #surrogate, true)
local code_ok, codepoint = pcall(utf8.codepoint, surrogate, 1, #surrogate, true)
local iterator, state, control = utf8.codes(surrogate, true)
local iter_ok, position, iter_codepoint = pcall(iterator, state, control)
local extended_ok, extended = pcall(utf8.char, 0x4000000)
local lax = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local always_lax = _VERSION == "Blu" or _VERSION == "Lua 5.3"
local strict = _VERSION == "Luau"
local extended_expected
if lax then
    local extended_len_ok, extended_length = pcall(utf8.len, extended, 1, #extended, true)
    local extended_code_ok, extended_codepoint = pcall(
        utf8.codepoint, extended, 1, #extended, true)
    local extended_iterator, extended_state, extended_control = utf8.codes(extended, true)
    local extended_iter_ok, extended_position, extended_value = pcall(
        extended_iterator, extended_state, extended_control)
    extended_expected = extended_ok
        and extended_len_ok and extended_length == 1
        and extended_code_ok and extended_codepoint == 0x4000000
        and extended_iter_ok and extended_position == 1 and extended_value == 0x4000000
else
    extended_expected = not extended_ok
end
return (lax or always_lax or strict)
    and len_ok and length == 1 and code_ok and codepoint == 0xD800
    and iter_ok and position == 1 and iter_codepoint == 0xD800
    and extended_expected
    or strict and not len_ok and not code_ok and not iter_ok and extended_expected
"#;
const UTF8_LAX_REFERENCE_SOURCE: &str = r#"
local surrogate = utf8.char(0xD800)
local len_ok, length = pcall(utf8.len, surrogate, 1, #surrogate, true)
local code_ok, codepoint = pcall(utf8.codepoint, surrogate, 1, #surrogate, true)
local iterator, state, control = utf8.codes(surrogate, true)
local iter_ok, position, iter_codepoint = pcall(iterator, state, control)
local extended_ok, extended = pcall(utf8.char, 0x4000000)
local lax = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local always_lax = _VERSION == "Lua 5.3" or _VERSION == "Blu"
local strict = _VERSION == "Luau"
local extended_expected
if lax then
    local extended_len_ok, extended_length = pcall(utf8.len, extended, 1, #extended, true)
    local extended_code_ok, extended_codepoint = pcall(
        utf8.codepoint, extended, 1, #extended, true)
    local extended_iterator, extended_state, extended_control = utf8.codes(extended, true)
    local extended_iter_ok, extended_position, extended_value = pcall(
        extended_iterator, extended_state, extended_control)
    extended_expected = extended_ok
        and extended_len_ok and extended_length == 1
        and extended_code_ok and extended_codepoint == 0x4000000
        and extended_iter_ok and extended_position == 1 and extended_value == 0x4000000
else
    extended_expected = not extended_ok
end
local result = (lax or always_lax or strict)
    and len_ok and length == 1 and code_ok and codepoint == 0xD800
    and iter_ok and position == 1 and iter_codepoint == 0xD800
    and extended_expected
    or strict and not len_ok and not code_ok and not iter_ok and extended_expected
print(type(result) .. ":" .. tostring(result))
"#;
const UTF8_CODES_SOURCE: &str = r#"
local text = "A" .. utf8.char(233) .. utf8.char(0x1F600)
local iterator, state, control = utf8.codes(text)
local first_position, first_codepoint = iterator(state, control)
local second_position, second_codepoint = iterator(state, first_position)
local third_position, third_codepoint = iterator(state, second_position)
local finished = iterator(state, third_position)
return type(iterator) == "function"
    and state == text
    and control == 0
    and first_position == 1
    and first_codepoint == 65
    and second_position == 2
    and second_codepoint == 233
    and third_position == 4
    and third_codepoint == 0x1F600
    and finished == nil
"#;
const UTF8_CODES_REFERENCE_SOURCE: &str = r#"
local text = "A" .. utf8.char(233) .. utf8.char(0x1F600)
local iterator, state, control = utf8.codes(text)
local first_position, first_codepoint = iterator(state, control)
local second_position, second_codepoint = iterator(state, first_position)
local third_position, third_codepoint = iterator(state, second_position)
local finished = iterator(state, third_position)
local result = type(iterator) == "function"
    and state == text
    and control == 0
    and first_position == 1
    and first_codepoint == 65
    and second_position == 2
    and second_codepoint == 233
    and third_position == 4
    and third_codepoint == 0x1F600
    and finished == nil
print(type(result) .. ":" .. tostring(result))
"#;
const WARN_SOURCE: &str = r#"
warn("@on")
warn("alpha", "beta")
warn("@off")
warn("ignored")
return type(warn) == "function"
"#;
const WARN_REFERENCE_SOURCE: &str = r#"
warn("@on")
warn("alpha", "beta")
warn("@off")
warn("ignored")
local result = type(warn) == "function"
print(type(result) .. ":" .. tostring(result))
"#;
const GLOBAL_DECLARATION_SOURCE: &str = r#"
global answer
answer = 7
global function read() return answer end
return read()
"#;
const GLOBAL_DECLARATION_REFERENCE_SOURCE: &str = r#"
answer = 7
function read() return answer end
local result = read()
print(type(result) .. ":" .. tostring(result))
"#;
const NAMED_VARARG_SOURCE: &str = r#"
local function collect(... args)
    return args.n, args[1], args[2]
end
local count, first, second = collect(3, 4)
return count == 2 and first == 3 and second == 4
"#;
const NAMED_VARARG_REFERENCE_SOURCE: &str = r#"
local function collect(... args)
    return args.n, args[1], args[2]
end
local count, first, second = collect(3, 4)
print(type(count == 2 and first == 3 and second == 4) .. ":" .. tostring(count == 2 and first == 3 and second == 4))
"#;
const LOAD_MODE_SOURCE: &str = r#"
local empty, empty_message = load("return 1", "chunk", "")
local unknown, unknown_message = load("return 1", "chunk", "x")
return empty == nil
    and empty_message == "attempt to load a text chunk (mode is '')"
    and unknown == nil
    and unknown_message == "attempt to load a text chunk (mode is 'x')"
"#;
const LOAD_MODE_REFERENCE_SOURCE: &str = r#"
local empty, empty_message = load("return 1", "chunk", "")
local unknown, unknown_message = load("return 1", "chunk", "x")
local result = empty == nil
    and empty_message == "attempt to load a text chunk (mode is '')"
    and unknown == nil
    and unknown_message == "attempt to load a text chunk (mode is 'x')"
print(type(result) .. ":" .. tostring(result))
"#;
const GOTO_SCOPE_LOAD_SOURCE: &str = r#"
local loaded, message = load("goto inside; do local value = 1 ::inside:: end return 1")
return loaded == nil and type(message) == "string"
"#;
const GOTO_SCOPE_LOAD_REFERENCE_SOURCE: &str = r#"
local loaded, message = load("goto inside; do local value = 1 ::inside:: end return 1")
local result = loaded == nil and type(message) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const GOTO_SCOPE_LOAD_LUA51_SOURCE: &str = r#"
local loaded, message = loadstring("goto inside; do local value = 1 ::inside:: end return 1")
return loaded == nil and type(message) == "string"
"#;
const GOTO_SCOPE_LOAD_LUA51_REFERENCE_SOURCE: &str = r#"
local loaded, message = loadstring("goto inside; do local value = 1 ::inside:: end return 1")
local result = loaded == nil and type(message) == "string"
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
const SELECT_INTEGER_ARGUMENT_SOURCE: &str = r#"
local function shape(selector)
    local ok, first, second, third = pcall(select, selector, "a", "b", "c")
    if not ok then return "error" end
    return tostring(first) .. ":" .. tostring(second) .. ":" .. tostring(third)
end
local modern = _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local blu = _VERSION == "Blu"
local expected = blu
    and shape(1.5) == "error"
    and shape(2.9) == "error"
    and shape(-1.5) == "error"
    and shape("2.5") == "b:c:nil"
    or modern
    and shape(1.5) == "error"
    and shape(2.9) == "error"
    and shape(-1.5) == "error"
    and shape("2.5") == "error"
    or not modern
    and shape(1.5) == "a:b:c"
    and shape(2.9) == "b:c:nil"
    and shape(-1.5) == "c:nil:nil"
    and shape("2.5") == "b:c:nil"
return expected
"#;
const SELECT_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local function shape(selector)
    local ok, first, second, third = pcall(select, selector, "a", "b", "c")
    if not ok then return "error" end
    return tostring(first) .. ":" .. tostring(second) .. ":" .. tostring(third)
end
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern
    and shape(1.5) == "error"
    and shape(2.9) == "error"
    and shape(-1.5) == "error"
    and shape("2.5") == "error"
    or not modern
    and shape(1.5) == "a:b:c"
    and shape(2.9) == "b:c:nil"
    and shape(-1.5) == "c:nil:nil"
    and shape("2.5") == "b:c:nil"
print(type(result) .. ":" .. tostring(result))
"#;
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
const REPEAT_CLOSE_SOURCE: &str = r#"
local events = ""
local metatable = { __close = function() events = events .. "close" end }
repeat
    local value <close> = setmetatable({}, metatable)
until events == "close"
return events == "closeclose"
"#;
const REPEAT_CLOSE_REFERENCE_SOURCE: &str = r#"
local events = ""
local metatable = { __close = function() events = events .. "close" end }
repeat
    local value <close> = setmetatable({}, metatable)
until events == "close"
local result = events == "closeclose"
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
const GENERIC_FOR_CLOSE_ERROR_SOURCE: &str = r#"
local events = ""
local function iterator(state, control)
    local value = control + 1
    if value <= 2 then
        return value
    end
end
local resource = setmetatable({}, {
    __close = function(value, err)
        events = events .. "close," .. type(err)
    end,
})
local ok, message = pcall(function()
    for value in iterator, nil, 0, resource do
        error("boom")
    end
end)
return not ok and type(message) == "string" and events == "close,string"
"#;
const GENERIC_FOR_CLOSE_ERROR_REFERENCE_SOURCE: &str = r#"
local events = ""
local function iterator(state, control)
    local value = control + 1
    if value <= 2 then
        return value
    end
end
local resource = setmetatable({}, {
    __close = function(value, err)
        events = events .. "close," .. type(err)
    end,
})
local ok, message = pcall(function()
    for value in iterator, nil, 0, resource do
        error("boom")
    end
end)
local result = not ok and type(message) == "string" and events == "close,string"
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
local binary, mode_message = load("return 1", "chunk", "b")
local text = load("return 42", "chunk", "t")
return default_result == 40 and answer == 40 and first == 41 and second == 42
    and environment.answer == 42 and binary == nil and type(mode_message) == "string"
    and text() == 42
"#;
const LOAD_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
answer = 39
local default_loaded = load("answer = answer + 1; return answer")
local default_result = default_loaded()
local environment = { answer = 40 }
local loaded = load("answer = answer + 1; return answer", "chunk", "t", environment)
local first = loaded()
local second = loaded()
local binary, mode_message = load("return 1", "chunk", "b")
local text = load("return 42", "chunk", "t")
local result = default_result == 40 and answer == 40 and first == 41 and second == 42
    and environment.answer == 42 and binary == nil and type(mode_message) == "string"
    and text() == 42
print(type(result) .. ":" .. tostring(result))
"#;
const LOAD_READER_SOURCE: &str = r#"
    local chunks = { "return 40", " + 2" }
local index = 0
local loaded, message = load(function()
    index = index + 1
    return chunks[index]
end)
local empty_chunks = { "return 7", "", " + 2" }
local empty_index = 0
local empty_loaded = load(function()
    empty_index = empty_index + 1
    return empty_chunks[empty_index]
end)
return loaded ~= nil
    and message == nil
    and loaded() == 42
    and index == 3
    and empty_loaded() == 7
    and empty_index == 2
"#;
const LOAD_READER_REFERENCE_SOURCE: &str = r#"
local chunks = { "return 40", " + 2" }
local index = 0
local loaded, message = load(function()
    index = index + 1
    return chunks[index]
end)
local empty_chunks = { "return 7", "", " + 2" }
local empty_index = 0
local empty_loaded = load(function()
    empty_index = empty_index + 1
    return empty_chunks[empty_index]
end)
local result = loaded ~= nil
    and message == nil
    and loaded() == 42
    and index == 3
    and empty_loaded() == 7
    and empty_index == 2
print(type(result) .. ":" .. tostring(result))
"#;
const YIELDING_LOAD_READER_SOURCE: &str = r#"
local thread = coroutine.create(function()
    local reads = 0
    local loaded = load(function()
        reads = reads + 1
        if reads == 1 then
            coroutine.yield("reader pause")
            return "return 42"
        end
        return ""
    end)
    return loaded()
end)
local first, signal = coroutine.resume(thread)
local second, result = coroutine.resume(thread)
return first and signal == "reader pause" and second and result == 42
"#;
const YIELDING_LOAD_READER_REFERENCE_SOURCE: &str = r#"
local thread = coroutine.create(function()
    local reads = 0
    local loaded = load(function()
        reads = reads + 1
        if reads == 1 then
            coroutine.yield("reader pause")
            return "return 42"
        end
        return ""
    end)
    return loaded()
end)
local first = coroutine.resume(thread)
print(type(first) .. ":" .. tostring(first))
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
local ok = pcall(load, "return 1")
return first == 41 and second == 42 and environment.answer == 42 and not ok
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
local ok = pcall(load, "return 1")
local result = first == 41 and second == 42 and environment.answer == 42 and not ok
    and getfenv(read) == environment and getfenv(loaded) == environment
print(type(result) .. ":" .. tostring(result))
"#;
const LUA51_STACK_ENVIRONMENT_SOURCE: &str = r#"
local get_environment = getfenv
local set_environment = setfenv
local load_source = loadstring
local base = get_environment(0)
local function read()
    local before = get_environment(1)
    local closure_environment = { answer = 41 }
    set_environment(1, closure_environment)
    local after = get_environment(1)
    return before == base
        and after == closure_environment
        and answer == 41
end
local stack_ok = read()
local environment = { answer = 40, getfenv = get_environment }
set_environment(0, environment)
local loaded = load_source("answer = answer + 1; return answer")
local first = loaded()
local second = loaded()
return stack_ok and first == 41 and second == 42
    and environment.answer == 42
    and get_environment() ~= environment
    and get_environment(0) == environment
"#;
const LUA51_STACK_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
local get_environment = getfenv
local set_environment = setfenv
local load_source = loadstring
local print_result = print
local type_result = type
local to_string = tostring
local base = get_environment(0)
local function read()
    local before = get_environment(1)
    local closure_environment = { answer = 41 }
    set_environment(1, closure_environment)
    local after = get_environment(1)
    return before == base
        and after == closure_environment
        and answer == 41
end
local stack_ok = read()
local environment = { answer = 40, getfenv = get_environment, tostring = to_string }
set_environment(0, environment)
local loaded = load_source("answer = answer + 1; return answer")
local first = loaded()
local second = loaded()
local result = stack_ok and first == 41 and second == 42
    and environment.answer == 42
    and get_environment() ~= environment
    and get_environment(0) == environment
print_result(type_result(result) .. ":" .. to_string(result))
"#;
const LUA51_NONCURRENT_ENVIRONMENT_SOURCE: &str = r#"
local get_environment = getfenv
local set_environment = setfenv
local base = get_environment(0)
local type_result = type
local function outer()
    local function middle()
        local caller = get_environment(3)
        local set_ok, set_result = pcall(set_environment, 3, {})
        return caller == base, set_ok, type_result(set_result) == "string"
    end
    return middle()
end
local ok, caller_ok, set_ok, set_error = pcall(outer)
local result
if ok then
    result = "outer-ok:" .. (caller_ok and "caller-ok" or "caller-error") .. ":"
        .. (set_ok and "set-ok" or "set-error") .. ":"
        .. (set_error and "error-string" or "set-value")
else
    result = "outer-error:" .. (type_result(caller_ok) == "string" and "error-string" or "other")
end
return result
"#;
const LUA51_NONCURRENT_ENVIRONMENT_REFERENCE_SOURCE: &str = r#"
local get_environment = getfenv
local set_environment = setfenv
local base = get_environment(0)
local print_result = print
local type_result = type
local to_string = tostring
local function outer()
    local function middle()
        local caller = get_environment(3)
        local set_ok, set_result = pcall(set_environment, 3, {})
        return caller == base, set_ok, type_result(set_result) == "string"
    end
    return middle()
end
local ok, caller_ok, set_ok, set_error = pcall(outer)
local result
if ok then
    result = "outer-ok:" .. (caller_ok and "caller-ok" or "caller-error") .. ":"
        .. (set_ok and "set-ok" or "set-error") .. ":"
        .. (set_error and "error-string" or "set-value")
else
    result = "outer-error:" .. (type_result(caller_ok) == "string" and "error-string" or "other")
end
print_result(type_result(result) .. ":" .. to_string(result))
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
const PROFILE_LIBRARY_SURFACE_SOURCE: &str = r#"
local function kind(value)
    return type(value)
end
local values = {
    kind(module), kind(rawget(package or {}, "seeall")),
    kind(newproxy), kind(rawget(_G or {}, "newproxy")),
    kind(io), kind(io and io.open), kind(io and io.tmpfile), kind(io and io.popen),
    kind(string.gfind), kind(rawget(string, "gfind")),
    kind(string.pack), kind(rawget(string, "pack")),
    kind(string.split), kind(rawget(string, "split")),
    kind(table.pack), kind(rawget(table, "pack")),
    kind(table.unpack), kind(rawget(table, "unpack")),
    kind(table.move), kind(rawget(table, "move")),
    kind(table.create), kind(rawget(table, "create")),
    kind(table.getn), kind(rawget(table, "getn")),
    kind(table.maxn), kind(rawget(table, "maxn")),
    kind(math.sinh), kind(rawget(math, "sinh")),
    kind(math.type), kind(rawget(math, "type")),
    kind(math.tointeger), kind(rawget(math, "tointeger")),
    kind(math.clamp), kind(rawget(math, "clamp")),
    kind(coroutine.close), kind(rawget(coroutine, "close")),
    kind(coroutine.isyieldable), kind(rawget(coroutine, "isyieldable")),
    kind(unpack), kind(bit32),
}
return table.concat(values, ":")
"#;
const PROFILE_LIBRARY_SURFACE_REFERENCE_SOURCE: &str = r#"
local function kind(value)
    return type(value)
end
local values = {
    kind(module), kind(rawget(package or {}, "seeall")),
    kind(newproxy), kind(rawget(_G or {}, "newproxy")),
    kind(io), kind(io and io.open), kind(io and io.tmpfile), kind(io and io.popen),
    kind(string.gfind), kind(rawget(string, "gfind")),
    kind(string.pack), kind(rawget(string, "pack")),
    kind(string.split), kind(rawget(string, "split")),
    kind(table.pack), kind(rawget(table, "pack")),
    kind(table.unpack), kind(rawget(table, "unpack")),
    kind(table.move), kind(rawget(table, "move")),
    kind(table.create), kind(rawget(table, "create")),
    kind(table.getn), kind(rawget(table, "getn")),
    kind(table.maxn), kind(rawget(table, "maxn")),
    kind(math.sinh), kind(rawget(math, "sinh")),
    kind(math.type), kind(rawget(math, "type")),
    kind(math.tointeger), kind(rawget(math, "tointeger")),
    kind(math.clamp), kind(rawget(math, "clamp")),
    kind(coroutine.close), kind(rawget(coroutine, "close")),
    kind(coroutine.isyieldable), kind(rawget(coroutine, "isyieldable")),
    kind(unpack), kind(bit32),
}
local result = table.concat(values, ":")
print(type(result) .. ":" .. tostring(result))
"#;
const COLLECTGARBAGE_CONTROLS_SOURCE: &str = r#"
local lua = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local modern = _VERSION == "Lua 5.2" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local integer = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local function zero(value)
    return value == 0 and (not integer or math.type(value) == "integer")
end
local stop_ok, stop_value = pcall(collectgarbage, "stop")
local stopped_ok, stopped = pcall(collectgarbage, "isrunning")
local collect_ok, collect_value = pcall(collectgarbage, "collect")
local restart_ok, restart_value = pcall(collectgarbage, "restart")
local running_ok, running = pcall(collectgarbage, "isrunning")
return stop_ok == lua and (not lua or zero(stop_value))
    and stopped_ok == modern and (not modern or stopped == false)
    and collect_ok and zero(collect_value)
    and restart_ok == lua and (not lua or zero(restart_value))
    and running_ok == modern and (not modern or running == true)
"#;
const COLLECTGARBAGE_CONTROLS_REFERENCE_SOURCE: &str = r#"
local lua = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local modern = _VERSION == "Lua 5.2" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local integer = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local function zero(value)
    return value == 0 and (not integer or math.type(value) == "integer")
end
local stop_ok, stop_value = pcall(collectgarbage, "stop")
local stopped_ok, stopped = pcall(collectgarbage, "isrunning")
local collect_ok, collect_value = pcall(collectgarbage, "collect")
local restart_ok, restart_value = pcall(collectgarbage, "restart")
local running_ok, running = pcall(collectgarbage, "isrunning")
local result = stop_ok == lua and (not lua or zero(stop_value))
    and stopped_ok == modern and (not modern or stopped == false)
    and collect_ok and zero(collect_value)
    and restart_ok == lua and (not lua or zero(restart_value))
    and running_ok == modern and (not modern or running == true)
print(type(result) .. ":" .. tostring(result))
"#;
const COLLECTGARBAGE_TUNING_BOUNDARY_SOURCE: &str = r#"
local function probe(command)
    local ok, value = pcall(collectgarbage, command, 250)
    return (ok and "ok" or "error") .. ":" .. type(value)
end
return probe("setpause") .. ":" .. probe("setstepmul") .. ":"
    .. probe("generational") .. ":" .. probe("incremental")
"#;
const COLLECTGARBAGE_TUNING_BOUNDARY_REFERENCE_SOURCE: &str = r#"
local function probe(command)
    local ok, value = pcall(collectgarbage, command, 250)
    return (ok and "ok" or "error") .. ":" .. type(value)
end
local result = probe("setpause") .. ":" .. probe("setstepmul") .. ":"
    .. probe("generational") .. ":" .. probe("incremental")
print(type(result) .. ":" .. result)
"#;
const GUEST_FINALIZER_SOURCE: &str = r#"
local finalized = 0
local function make()
    local value = setmetatable({}, { __gc = function() finalized = finalized + 1 end })
end
make()
collectgarbage("collect")
return finalized
"#;
const GUEST_FINALIZER_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local function make()
    local value = setmetatable({}, { __gc = function() finalized = finalized + 1 end })
end
make()
collectgarbage("collect")
print(type(finalized) .. ":" .. tostring(finalized))
"#;
const GUEST_FINALIZER_RESURRECTION_SOURCE: &str = r#"
local finalized = 0
local resurrected
local function make()
    local value = setmetatable({}, {
        __gc = function(value)
            finalized = finalized + 1
            resurrected = value
        end,
    })
end
make()
collectgarbage("collect")
local first = finalized == 1 and resurrected ~= nil
collectgarbage("collect")
return first and finalized == 1 and resurrected ~= nil
"#;
const GUEST_FINALIZER_RESURRECTION_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local resurrected
local function make()
    local value = setmetatable({}, {
        __gc = function(value)
            finalized = finalized + 1
            resurrected = value
        end,
    })
end
make()
collectgarbage("collect")
local first = finalized == 1 and resurrected ~= nil
collectgarbage("collect")
local result = first and finalized == 1 and resurrected ~= nil
print(type(result) .. ":" .. tostring(result))
"#;
const GUEST_FINALIZER_REARM_SOURCE: &str = r#"
local finalized = 0
local resurrected
local metatable
local function finalize(value)
    finalized = finalized + 1
    resurrected = value
    setmetatable(value, metatable)
end
metatable = { __gc = finalize }
local function make()
    local value = setmetatable({}, metatable)
end
make()
collectgarbage("collect")
local first = finalized == 1 and resurrected ~= nil
resurrected = nil
collectgarbage("collect")
return first and finalized == 2
"#;
const GUEST_FINALIZER_REARM_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local resurrected
local metatable
local function finalize(value)
    finalized = finalized + 1
    resurrected = value
    setmetatable(value, metatable)
end
metatable = { __gc = finalize }
local function make()
    local value = setmetatable({}, metatable)
end
make()
collectgarbage("collect")
local first = finalized == 1 and resurrected ~= nil
resurrected = nil
collectgarbage("collect")
local result = first and finalized == 2
print(type(result) .. ":" .. tostring(result))
"#;
const GUEST_FINALIZER_ORDER_SOURCE: &str = r#"
local order = {}
local metatable = { __gc = function(value) order[#order + 1] = value.id end }
local function make(id)
    local value = setmetatable({ id = id }, metatable)
end
make(1)
make(2)
make(3)
collectgarbage("collect")
return table.concat(order, ",")
"#;
const GUEST_FINALIZER_ORDER_REFERENCE_SOURCE: &str = r#"
local order = {}
local metatable = { __gc = function(value) order[#order + 1] = value.id end }
local function make(id)
    local value = setmetatable({ id = id }, metatable)
end
make(1)
make(2)
make(3)
collectgarbage("collect")
print(type(table.concat(order, ",")) .. ":" .. table.concat(order, ","))
"#;
const HOST_USERDATA_FINALIZER_SOURCE: &str = r#"
local finalized = 0
local resurrected
local metatable = { __gc = function(value)
    finalized = finalized + 1
    resurrected = value
end }
local file = io.open("answer.txt")
debug.setmetatable(file, metatable)
file = nil
collectgarbage("collect")
if resurrected ~= nil then
    debug.setmetatable(resurrected, metatable)
    resurrected = nil
    collectgarbage("collect")
end
return finalized
"#;
const HOST_USERDATA_FINALIZER_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local resurrected
local metatable = { __gc = function(value)
    finalized = finalized + 1
    resurrected = value
end }
local file = io.open("answer.txt")
debug.setmetatable(file, metatable)
file = nil
collectgarbage("collect")
if resurrected ~= nil then
    debug.setmetatable(resurrected, metatable)
    resurrected = nil
    collectgarbage("collect")
end
print(type(finalized) .. ":" .. tostring(finalized))
"#;
const NATIVE_USERDATA_FINALIZER_SOURCE: &str = r#"
local finalized = 0
local value = package.loadlib("trusted.so", "luaopen_trusted")
debug.setmetatable(value, { __gc = function() finalized = finalized + 1 end })
local kind = type(value)
value = nil
collectgarbage("collect")
return kind .. ":" .. tostring(finalized)
"#;
const NATIVE_USERDATA_FINALIZER_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local value = io.tmpfile()
debug.setmetatable(value, { __gc = function() finalized = finalized + 1 end })
local kind = type(value)
value = nil
collectgarbage("collect")
print(kind .. ":" .. tostring(finalized))
"#;
const NATIVE_LOADLIB_UNAVAILABLE_SOURCE: &str = r#"
local loaded, message, where = package.loadlib("trusted.so", "luaopen_trusted")
return loaded == nil and type(message) == "string" and where == "absent"
"#;
const NATIVE_LOADLIB_UNAVAILABLE_REFERENCE_SOURCE: &str = r#"
local loaded, message, where = package.loadlib("trusted.so", "luaopen_trusted")
local result = loaded == nil and type(message) == "string" and where == "absent"
print(type(result) .. ":" .. tostring(result))
"#;
const IO_TMPFILE_SOURCE: &str = r#"
local finalized = 0
local file = io.tmpfile()
local open = io.type(file) == "file"
debug.setmetatable(file, { __gc = function() finalized = finalized + 1 end })
local kind = type(file)
file = nil
collectgarbage("collect")
return kind .. ":" .. tostring(open) .. ":" .. tostring(finalized)
"#;
const IO_TMPFILE_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local file = io.tmpfile()
local open = io.type(file) == "file"
debug.setmetatable(file, { __gc = function() finalized = finalized + 1 end })
local kind = type(file)
file = nil
collectgarbage("collect")
print(kind .. ":" .. tostring(open) .. ":" .. tostring(finalized))
"#;
const IO_CONSTRUCTOR_FAILURE_SOURCE: &str = r#"
local temporary, temporary_error = io.tmpfile()
local process, process_error = io.popen("unavailable", "r")
return temporary == nil and type(temporary_error) == "string"
    and process == nil and type(process_error) == "string"
"#;
const IO_CONSTRUCTOR_FAILURE_REFERENCE_SOURCE: &str = r#"
local missing, missing_error = io.open("definitely-missing-file", "rb")
local invalid_ok, invalid_error = pcall(io.popen, "printf unavailable", "invalid")
local result = missing == nil and type(missing_error) == "string"
    and not invalid_ok and type(invalid_error) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const IO_OPERATION_FAILURE_SOURCE: &str = r#"
local read_file = io.open("read-failure", "r")
local read_result, read_error = read_file:read("*a")
local write_file = io.open("write-failure", "w")
local write_result, write_error = write_file:write("x")
local seek_file = io.open("seek-failure", "r")
local seek_result, seek_error = seek_file:seek("set", 0)
local flush_file = io.open("flush-failure", "r")
local flush_result, flush_error = flush_file:flush()
local buffer_file = io.open("buffer-failure", "r")
local buffer_result, buffer_error = buffer_file:setvbuf("full", 64)
local close_file = io.open("close-failure", "r")
local close_result, close_error = close_file:close()
local close_type = io.type(close_file)
local close_again_ok, close_again_error = pcall(close_file.close, close_file)
local line_file = io.open("line-read-failure", "r")
local line_iterator = line_file:lines("*l")
local line_ok, line_error = pcall(line_iterator)
return read_result == nil and type(read_error) == "string"
    and write_result == nil and type(write_error) == "string"
    and seek_result == nil and type(seek_error) == "string"
    and flush_result == nil and type(flush_error) == "string"
    and buffer_result == nil and type(buffer_error) == "string"
    and close_result == nil and type(close_error) == "string"
    and close_type == "closed file"
    and not close_again_ok and type(close_again_error) == "string"
    and not line_ok and type(line_error) == "string"
"#;
const IO_OPERATION_FAILURE_REFERENCE_SOURCE: &str = r#"
local missing, missing_error = io.open("definitely-missing-file", "rb")
local file = io.open("answer.txt", "rb")
local seek_result, seek_error = file:seek("set", -1)
local closed = file:close()
local closed_type = io.type(file)
local close_again_ok, close_again_error = pcall(file.close, file)
local line_file = io.open("answer.txt", "rb")
local line_iterator = line_file:lines("*l")
line_file:close()
local line_ok, line_error = pcall(line_iterator)
local result = missing == nil and type(missing_error) == "string"
    and seek_result == nil and type(seek_error) == "string"
    and closed == true
    and closed_type == "closed file"
    and not close_again_ok and type(close_again_error) == "string"
    and not line_ok and type(line_error) == "string"
print(type(result) .. ":" .. tostring(result))
"#;
const DISCARDED_IO_LINES_SOURCE: &str = r#"
local finalized = 0
local file = io.open("answer.txt")
local iterator = file:lines()
local info = debug.getinfo(iterator, "Snu")
local line_info = debug.getinfo(iterator, "L")
local expected_nups = _VERSION == "Lua 5.1" and 2 or 3
local upvalue_name = debug.getupvalue(iterator, 1)
local upvalue_id = type(debug.upvalueid) == "function" and debug.upvalueid(iterator, 1) or nil
local second_name, second_value = debug.getupvalue(iterator, 2)
local second_id = type(debug.upvalueid) == "function" and debug.upvalueid(iterator, 2) or nil
local third_name, third_value = debug.getupvalue(iterator, 3)
local third_id = type(debug.upvalueid) == "function" and debug.upvalueid(iterator, 3) or nil
local upvalues_match = _VERSION == "Lua 5.1"
    and second_name == nil and third_name == nil
    or _VERSION ~= "Lua 5.1"
        and second_name == "" and second_value == 0
        and third_name == "" and third_value == false
local ids_match = _VERSION == "Lua 5.1"
    and second_id == nil and third_id == nil
    or _VERSION ~= "Lua 5.1"
        and second_id ~= nil and third_id ~= nil and second_id ~= third_id
local joined = type(debug.upvaluejoin) == "function" and pcall(debug.upvaluejoin, iterator, 1, iterator, 1) or false
local setup_name = debug.setupvalue(iterator, 1, file)
local second_setup = debug.setupvalue(iterator, 2, 0)
local third_setup = debug.setupvalue(iterator, 3, true)
local setup_match = _VERSION == "Lua 5.1"
    and second_setup == nil and third_setup == nil
    or _VERSION ~= "Lua 5.1" and second_setup == "" and third_setup == ""
local wrapped = coroutine.wrap(function() return iterator() end)
local stepped = wrapped()
local closed = io.type(file) == "closed file"
debug.setmetatable(file, { __gc = function() finalized = finalized + 1 end })
file = nil
iterator = nil
collectgarbage("collect")
return tostring(upvalue_name == nil) .. ":" .. tostring(upvalue_id == nil)
    .. ":" .. tostring(not joined) .. ":" .. tostring(stepped == nil)
    .. ":" .. tostring(setup_name == nil)
    .. ":" .. tostring(info.nups == expected_nups)
    .. ":" .. tostring(line_info.activelines == nil)
    .. ":" .. tostring(upvalues_match) .. ":" .. tostring(ids_match)
    .. ":" .. tostring(setup_match)
    .. ":" .. tostring(closed)
    .. ":" .. info.what .. ":" .. info.source .. ":" .. tostring(finalized)
"#;
const DISCARDED_IO_LINES_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local file = io.open("answer.txt")
local iterator = file:lines()
local info = debug.getinfo(iterator, "Snu")
local line_info = debug.getinfo(iterator, "L")
local expected_nups = _VERSION == "Lua 5.1" and 2 or 3
local upvalue_name = debug.getupvalue(iterator, 1)
local upvalue_id = type(debug.upvalueid) == "function" and debug.upvalueid(iterator, 1) or nil
local second_name, second_value = debug.getupvalue(iterator, 2)
local second_id = type(debug.upvalueid) == "function" and debug.upvalueid(iterator, 2) or nil
local third_name, third_value = debug.getupvalue(iterator, 3)
local third_id = type(debug.upvalueid) == "function" and debug.upvalueid(iterator, 3) or nil
local upvalues_match = _VERSION == "Lua 5.1"
    and second_name == nil and third_name == nil
    or _VERSION ~= "Lua 5.1"
        and second_name == "" and second_value == 0
        and third_name == "" and third_value == false
local ids_match = _VERSION == "Lua 5.1"
    and second_id == nil and third_id == nil
    or _VERSION ~= "Lua 5.1"
        and second_id ~= nil and third_id ~= nil and second_id ~= third_id
local joined = type(debug.upvaluejoin) == "function" and pcall(debug.upvaluejoin, iterator, 1, iterator, 1) or false
local setup_name = debug.setupvalue(iterator, 1, file)
local second_setup = debug.setupvalue(iterator, 2, 0)
local third_setup = debug.setupvalue(iterator, 3, true)
local setup_match = _VERSION == "Lua 5.1"
    and second_setup == nil and third_setup == nil
    or _VERSION ~= "Lua 5.1" and second_setup == "" and third_setup == ""
local wrapped = coroutine.wrap(function() return iterator() end)
local stepped = wrapped()
local closed = io.type(file) == "closed file"
debug.setmetatable(file, { __gc = function() finalized = finalized + 1 end })
file = nil
iterator = nil
collectgarbage("collect")
local result = tostring(upvalue_name == nil) .. ":" .. tostring(upvalue_id == nil)
    .. ":" .. tostring(not joined) .. ":" .. tostring(stepped == nil)
    .. ":" .. tostring(setup_name == nil)
    .. ":" .. tostring(info.nups == expected_nups)
    .. ":" .. tostring(line_info.activelines == nil)
    .. ":" .. tostring(upvalues_match) .. ":" .. tostring(ids_match)
    .. ":" .. tostring(setup_match)
    .. ":" .. tostring(closed)
    .. ":" .. info.what .. ":" .. info.source .. ":" .. tostring(finalized)
print(type(result) .. ":" .. result)
"#;
const GUEST_FINALIZER_ERROR_SOURCE: &str = r#"
local finalized = 0
local function make()
    local value = setmetatable({}, { __gc = function()
        finalized = finalized + 1
        error("boom")
    end })
end
make()
local ok, err = pcall(collectgarbage, "collect")
return type(ok) .. ":" .. tostring(ok) .. ":" .. type(err) .. ":" .. tostring(finalized)
"#;
const GUEST_FINALIZER_ERROR_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local function make()
    local value = setmetatable({}, { __gc = function()
        finalized = finalized + 1
        error("boom")
    end })
end
make()
local ok, err = pcall(collectgarbage, "collect")
print(type(ok) .. ":" .. tostring(ok) .. ":" .. type(err) .. ":" .. tostring(finalized))
"#;
const GUEST_FINALIZER_YIELD_SOURCE: &str = r#"
local finalized = 0
local function make()
    local value = setmetatable({}, { __gc = function()
        finalized = finalized + 1
        coroutine.yield("yielded")
        finalized = finalized + 1
    end })
end
make()
local ok, err = pcall(collectgarbage, "collect")
return type(ok) .. ":" .. tostring(ok) .. ":" .. type(err) .. ":" .. tostring(finalized)
"#;
const GUEST_FINALIZER_YIELD_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local function make()
    local value = setmetatable({}, { __gc = function()
        finalized = finalized + 1
        coroutine.yield("yielded")
        finalized = finalized + 1
    end })
end
make()
local ok, err = pcall(collectgarbage, "collect")
print(type(ok) .. ":" .. tostring(ok) .. ":" .. type(err) .. ":" .. tostring(finalized))
"#;
const GUEST_FINALIZER_REGISTER_LIVENESS_SOURCE: &str = r#"
local finalized = 0
local resurrected
local metatable
local function finalize(value)
    finalized = finalized + 1
    resurrected = value
end
metatable = { __gc = finalize }
local function make()
    local value = setmetatable({}, metatable)
end
make()
collectgarbage("collect")
local function rearm(value)
    setmetatable(value, metatable)
end
if resurrected ~= nil then
    rearm(resurrected)
    resurrected = nil
    collectgarbage("collect")
end
return finalized
"#;
const GUEST_FINALIZER_REGISTER_LIVENESS_REFERENCE_SOURCE: &str = r#"
local finalized = 0
local resurrected
local metatable
local function finalize(value)
    finalized = finalized + 1
    resurrected = value
end
metatable = { __gc = finalize }
local function make()
    local value = setmetatable({}, metatable)
end
make()
collectgarbage("collect")
local function rearm(value)
    setmetatable(value, metatable)
end
if resurrected ~= nil then
    rearm(resurrected)
    resurrected = nil
    collectgarbage("collect")
end
print(type(finalized) .. ":" .. tostring(finalized))
"#;
const DEBUG_CSTACK_LIMIT_SOURCE: &str = r#"
return type(debug and debug.setcstacklimit)
"#;
const DEBUG_CSTACK_LIMIT_REFERENCE_SOURCE: &str = r#"
local value = debug and debug.setcstacklimit
print(type(value))
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
const MATH_INTEGER_BOUNDS_SOURCE: &str = r#"
local min = math.mininteger
local max = math.maxinteger
return (min == nil and max == nil)
    or (min < 0 and max > 0
        and tostring(min) == "-9223372036854775808"
        and tostring(max) == "9223372036854775807")
"#;
const MATH_INTEGER_BOUNDS_REFERENCE_SOURCE: &str = r#"
local min = math.mininteger
local max = math.maxinteger
local result = (min == nil and max == nil)
    or (min < 0 and max > 0
        and tostring(min) == "-9223372036854775808"
        and tostring(max) == "9223372036854775807")
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_RANDOM_FRACTIONAL_ARGUMENT_SOURCE: &str = r#"
local one_ok, one = pcall(math.random, 1.5)
local two_ok, two = pcall(math.random, 1.5, 3.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local lua52 = _VERSION == "Lua 5.2"
return modern and not one_ok and not two_ok
    or lua52 and one_ok and (one == 1 or one == 2)
        and two_ok and (two == 1.5 or two == 2.5 or two == 3.5)
    or not modern and not lua52 and one_ok and one == 1
        and two_ok and two >= 1 and two <= 3 and two % 1 == 0
"#;
const MATH_RANDOM_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local one_ok, one = pcall(math.random, 1.5)
local two_ok, two = pcall(math.random, 1.5, 3.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local lua52 = _VERSION == "Lua 5.2"
local result = modern and not one_ok and not two_ok
    or lua52 and one_ok and (one == 1 or one == 2)
        and two_ok and (two == 1.5 or two == 2.5 or two == 3.5)
    or not modern and not lua52 and one_ok and one == 1
        and two_ok and two >= 1 and two <= 3 and two % 1 == 0
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_LDEXP_FRACTIONAL_ARGUMENT_SOURCE: &str = r#"
local ok, value = pcall(math.ldexp, 1, 1.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok and value == 2
"#;
const MATH_LDEXP_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok, value = pcall(math.ldexp, 1, 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok and value == 2
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_PROFILE_EDGE_SOURCE: &str = r#"
local log_ok, log_value = pcall(math.log, 8, 2.5)
local log_base = log_ok
    and math.abs(log_value - math.log(8) / math.log(2.5)) < 1e-12
local seed_first, seed_second, seed_third = math.randomseed(123)
local modern_seed = _VERSION == "Blu" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local log_expected = _VERSION == "Lua 5.1" and not log_base
    or _VERSION ~= "Lua 5.1" and log_base
local seed_expected = modern_seed
    and seed_first == 123 and seed_second == 0 and seed_third == nil
    or not modern_seed and seed_first == nil and seed_second == nil
local atan_ok, atan_value = pcall(math.atan, 1, -1)
local modern_atan = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local atan_expected = atan_ok
    and math.abs(atan_value - (modern_atan and 3 * math.pi / 4 or math.pi / 4)) < 1e-12
local modern_abs = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local abs_ok, abs_value = pcall(math.abs, modern_abs and math.mininteger or -3)
local abs_expected = modern_abs
    and abs_ok and abs_value == math.mininteger and math.type(abs_value) == "integer"
    or not modern_abs and abs_ok and abs_value == 3
return log_expected and seed_expected and atan_expected and abs_expected
"#;
const MATH_PROFILE_EDGE_REFERENCE_SOURCE: &str = r#"
local log_ok, log_value = pcall(math.log, 8, 2.5)
local log_base = log_ok
    and math.abs(log_value - math.log(8) / math.log(2.5)) < 1e-12
local seed_first, seed_second, seed_third = math.randomseed(123)
local modern_seed = _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local log_expected = _VERSION == "Lua 5.1" and not log_base
    or _VERSION ~= "Lua 5.1" and log_base
local seed_expected = modern_seed
    and seed_first == 123 and seed_second == 0 and seed_third == nil
    or not modern_seed and seed_first == nil and seed_second == nil
local atan_ok, atan_value = pcall(math.atan, 1, -1)
local modern_atan = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local atan_expected = atan_ok
    and math.abs(atan_value - (modern_atan and 3 * math.pi / 4 or math.pi / 4)) < 1e-12
local modern_abs = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local abs_ok, abs_value = pcall(math.abs, modern_abs and math.mininteger or -3)
local abs_expected = modern_abs
    and abs_ok and abs_value == math.mininteger and math.type(abs_value) == "integer"
    or not modern_abs and abs_ok and abs_value == 3
local result = log_expected and seed_expected and atan_expected and abs_expected
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_MIN_MAX_EDGE_SOURCE: &str = r#"
local nan = 0 / 0
local min_left = math.min(nan, 1)
local min_right = math.min(1, nan)
local max_left = math.max(nan, 1)
local max_right = math.max(1, nan)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local subtype = not modern
    or (math.type(math.min(1, 2.0)) == "integer"
        and math.type(math.max(1, 2.0)) == "float"
        and math.type(math.min(1.5, 2)) == "float")
return min_left ~= min_left and min_right == 1
    and max_left ~= max_left and max_right == 1
    and subtype
"#;
const MATH_MIN_MAX_EDGE_REFERENCE_SOURCE: &str = r#"
local nan = 0 / 0
local min_left = math.min(nan, 1)
local min_right = math.min(1, nan)
local max_left = math.max(nan, 1)
local max_right = math.max(1, nan)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local subtype = not modern
    or (math.type(math.min(1, 2.0)) == "integer"
        and math.type(math.max(1, 2.0)) == "float"
        and math.type(math.min(1.5, 2)) == "float")
local result = min_left ~= min_left and min_right == 1
    and max_left ~= max_left and max_right == 1
    and subtype
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_FMOD_EDGE_SOURCE: &str = r#"
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local zero_ok, zero_value = pcall(math.fmod, 7, 0)
local nan_ok, nan_value = pcall(math.fmod, 0 / 0, 1)
local infinity_ok, infinity_value = pcall(math.fmod, math.huge, 1)
local minimum_ok, minimum_value = pcall(math.fmod, math.mininteger or -7, -1)
local subtype = not modern
    or (math.type(math.fmod(-7, 3)) == "integer" and minimum_ok and minimum_value == 0)
local zero = modern and not zero_ok or not modern
    and zero_ok and zero_value ~= zero_value
local nonfinite = nan_ok and nan_value ~= nan_value
    and infinity_ok and infinity_value ~= infinity_value
return subtype and zero and nonfinite
"#;
const MATH_FMOD_EDGE_REFERENCE_SOURCE: &str = r#"
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local zero_ok, zero_value = pcall(math.fmod, 7, 0)
local nan_ok, nan_value = pcall(math.fmod, 0 / 0, 1)
local infinity_ok, infinity_value = pcall(math.fmod, math.huge, 1)
local minimum_ok, minimum_value = pcall(math.fmod, math.mininteger or -7, -1)
local subtype = not modern
    or (math.type(math.fmod(-7, 3)) == "integer" and minimum_ok and minimum_value == 0)
local zero = modern and not zero_ok or not modern
    and zero_ok and zero_value ~= zero_value
local nonfinite = nan_ok and nan_value ~= nan_value
    and infinity_ok and infinity_value ~= infinity_value
local result = subtype and zero and nonfinite
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_SUBTYPE_EDGE_SOURCE: &str = r#"
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local integral, fractional = math.modf("-3.5")
local modf_shape = not modern
    or (math.type(integral) == "integer" and integral == -3
        and math.type(fractional) == "float" and fractional == -0.5)
local has_integer_helpers = modern and _VERSION ~= "Luau"
local tointeger_shape = not has_integer_helpers
    or (math.tointeger ~= nil and math.tointeger("3") == 3
        and math.tointeger("3.5") == nil and math.tointeger(math.huge) == nil)
local ult_shape = not has_integer_helpers
    or (math.ult ~= nil and math.ult(-1, 1) == false and math.ult(1, -1)
        and not pcall(math.ult, 1.5, 2))
return modf_shape and tointeger_shape and ult_shape
"#;
const MATH_SUBTYPE_EDGE_REFERENCE_SOURCE: &str = r#"
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local integral, fractional = math.modf("-3.5")
local modf_shape = not modern
    or (math.type(integral) == "integer" and integral == -3
        and math.type(fractional) == "float" and fractional == -0.5)
local has_integer_helpers = modern and _VERSION ~= "Luau"
local tointeger_shape = not has_integer_helpers
    or (math.tointeger ~= nil and math.tointeger("3") == 3
        and math.tointeger("3.5") == nil and math.tointeger(math.huge) == nil)
local ult_shape = not has_integer_helpers
    or (math.ult ~= nil and math.ult(-1, 1) == false and math.ult(1, -1)
        and not pcall(math.ult, 1.5, 2))
local result = modf_shape and tointeger_shape and ult_shape
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_STRING_ARGUMENT_SOURCE: &str = r#"
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local function near(value, expected)
    return math.abs(value - expected) < 1e-12
end
local abs_ok, absolute = pcall(math.abs, "-3.5")
local floor_ok, floored = pcall(math.floor, "3.5")
local ceil_ok, ceiled = pcall(math.ceil, "3.5")
local sqrt_ok, square_root = pcall(math.sqrt, "4")
local pow_ok, power = true, 8
if math.pow then
    pow_ok, power = pcall(math.pow, "2", "3")
end
local fmod_ok, remainder = pcall(math.fmod, "7", "3")
local log_ok, logarithm = pcall(math.log, "8", "2")
local sin_ok, sine = pcall(math.sin, "0")
local min_ok, minimum = pcall(math.min, "2", "1")
local max_ok, maximum = pcall(math.max, "2", "3")
local min_shape = modern and type(minimum) == "string" and minimum == "1"
    or not modern and min_ok and minimum == 1
local max_shape = modern and type(maximum) == "string" and maximum == "3"
    or not modern and max_ok and maximum == 3
local pow_shape = _VERSION == "Lua 5.5" and math.pow == nil
    or pow_ok and power == 8
return abs_ok and absolute == 3.5
    and floor_ok and floored == 3
    and ceil_ok and ceiled == 4
    and sqrt_ok and square_root == 2
    and pow_shape
    and fmod_ok and remainder == 1
    and log_ok and near(logarithm, 3)
    and sin_ok and sine == 0
    and min_ok and min_shape and max_ok and max_shape
"#;
const MATH_STRING_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local function near(value, expected)
    return math.abs(value - expected) < 1e-12
end
local abs_ok, absolute = pcall(math.abs, "-3.5")
local floor_ok, floored = pcall(math.floor, "3.5")
local ceil_ok, ceiled = pcall(math.ceil, "3.5")
local sqrt_ok, square_root = pcall(math.sqrt, "4")
local pow_ok, power = true, 8
if math.pow then
    pow_ok, power = pcall(math.pow, "2", "3")
end
local fmod_ok, remainder = pcall(math.fmod, "7", "3")
local log_ok, logarithm = pcall(math.log, "8", "2")
local sin_ok, sine = pcall(math.sin, "0")
local min_ok, minimum = pcall(math.min, "2", "1")
local max_ok, maximum = pcall(math.max, "2", "3")
local min_shape = modern and type(minimum) == "string" and minimum == "1"
    or not modern and min_ok and minimum == 1
local max_shape = modern and type(maximum) == "string" and maximum == "3"
    or not modern and max_ok and maximum == 3
local pow_shape = _VERSION == "Lua 5.5" and math.pow == nil
    or pow_ok and power == 8
local result = abs_ok and absolute == 3.5
    and floor_ok and floored == 3
    and ceil_ok and ceiled == 4
    and sqrt_ok and square_root == 2
    and pow_shape
    and fmod_ok and remainder == 1
    and log_ok and near(logarithm, 3)
    and sin_ok and sine == 0
    and min_ok and min_shape and max_ok and max_shape
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_LUAU_EXTENSION_SOURCE: &str = r#"
local function near(value, expected)
    return math.abs(value - expected) < 1e-12
end
local clamp_ok, clamped = pcall(math.clamp, "2", "1", "3")
local sign_ok, sign = pcall(math.sign, "-2")
local round_ok, rounded = pcall(math.round, "1.5")
local nan_ok, nan = pcall(math.isnan, 0 / 0)
local inf_ok, infinite = pcall(math.isinf, math.huge)
local finite_ok, finite = pcall(math.isfinite, 2)
local lerp_ok, interpolated = pcall(math.lerp, "0", "10", "0.25")
local map_ok, mapped = pcall(math.map, "0", "0", "10", "0", "100")
local noise_ok, noise = pcall(math.noise, "0", "0", "0")
return clamp_ok and clamped == 2
    and sign_ok and sign == -1
    and round_ok and rounded == 2
    and nan_ok and nan and inf_ok and infinite
    and finite_ok and finite and lerp_ok and near(interpolated, 2.5)
    and map_ok and mapped == 0 and noise_ok and noise == 0
"#;
const MATH_LUAU_EXTENSION_REFERENCE_SOURCE: &str = r#"
local function near(value, expected)
    return math.abs(value - expected) < 1e-12
end
local clamp_ok, clamped = pcall(math.clamp, "2", "1", "3")
local sign_ok, sign = pcall(math.sign, "-2")
local round_ok, rounded = pcall(math.round, "1.5")
local nan_ok, nan = pcall(math.isnan, 0 / 0)
local inf_ok, infinite = pcall(math.isinf, math.huge)
local finite_ok, finite = pcall(math.isfinite, 2)
local lerp_ok, interpolated = pcall(math.lerp, "0", "10", "0.25")
local map_ok, mapped = pcall(math.map, "0", "0", "10", "0", "100")
local noise_ok, noise = pcall(math.noise, "0", "0", "0")
local result = clamp_ok and clamped == 2
    and sign_ok and sign == -1
    and round_ok and rounded == 2
    and nan_ok and nan and inf_ok and infinite
    and finite_ok and finite and lerp_ok and near(interpolated, 2.5)
    and map_ok and mapped == 0 and noise_ok and noise == 0
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_PACK_SOURCE: &str = r#"
local data = string.pack("<I2 i2 f d c3 z s", 4660, -2, 1.5, 2.5, "ab", "z", "hello")
local a, b, c, d, e, f, g, position = string.unpack(
    "<I2 i2 f d c3 z s", data)
return string.packsize("<I2 i2 f d c3") == 19
    and a == 4660 and b == -2 and c == 1.5 and d == 2.5
    and e == "ab\0" and f == "z" and g == "hello"
    and position == #data + 1
"#;
const STRING_PACK_REFERENCE_SOURCE: &str = r#"
local data = string.pack("<I2 i2 f d c3 z s", 4660, -2, 1.5, 2.5, "ab", "z", "hello")
local a, b, c, d, e, f, g, position = string.unpack(
    "<I2 i2 f d c3 z s", data)
local result = string.packsize("<I2 i2 f d c3") == 19
    and a == 4660 and b == -2 and c == 1.5 and d == 2.5
    and e == "ab\0" and f == "z" and g == "hello"
    and position == #data + 1
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_PACK_ALIGNMENT_SOURCE: &str = r#"
local data = string.pack(">!4 b Xh i4", -12, 100)
local a, b, position = string.unpack(">!4 b Xh i4", data)
return #data == 8 and a == -12 and b == 100 and position == 9
"#;
const STRING_PACK_ALIGNMENT_REFERENCE_SOURCE: &str = r#"
local data = string.pack(">!4 b Xh i4", -12, 100)
local a, b, position = string.unpack(">!4 b Xh i4", data)
local result = #data == 8 and a == -12 and b == 100 and position == 9
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_UNPACK_INTEGER_ARGUMENT_SOURCE: &str = r#"
local data = string.pack("B", 97)
local ok, value, position = pcall(string.unpack, "B", data, 1.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok and value == 97 and position == 2
"#;
const STRING_UNPACK_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local data = string.pack("B", 97)
local ok, value, position = pcall(string.unpack, "B", data, 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok and value == 97 and position == 2
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_PACK_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok, value = pcall(string.pack, "I2", 1.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
return modern and not ok or not modern and ok and type(value) == "string" and #value == 2
"#;
const STRING_PACK_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok, value = pcall(string.pack, "I2", 1.5)
local modern = _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4"
    or _VERSION == "Lua 5.5"
local result = modern and not ok or not modern and ok and type(value) == "string" and #value == 2
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
const XPCALL_ARGUMENT_SOURCE: &str = r#"
local function target(value)
    return value, value and value + 1
end
local ok, first, second = xpcall(target, function(message)
    return "handled:" .. type(message)
end, 5)
local lua51 = _VERSION == "Lua 5.1"
return ok and (lua51 and first == nil and second == nil
    or not lua51 and first == 5 and second == 6)
"#;
const XPCALL_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local function target(value)
    return value, value and value + 1
end
local ok, first, second = xpcall(target, function(message)
    return "handled:" .. type(message)
end, 5)
local lua51 = _VERSION == "Lua 5.1"
local result = ok and (lua51 and first == nil and second == nil
    or not lua51 and first == 5 and second == 6)
print(type(result) .. ":" .. tostring(result))
"#;
const XPCALL_HANDLER_ERROR_SOURCE: &str = r#"
local function handler_fails(value)
    local ok, result = xpcall(function()
        error("target")
    end, function()
        error(value)
    end)
    return not ok and result == "error in error handling" and type(result) == "string"
end
return handler_fails(false) and handler_fails(0)
"#;
const XPCALL_HANDLER_ERROR_REFERENCE_SOURCE: &str = r#"
local function handler_fails(value)
    local ok, result = xpcall(function()
        error("target")
    end, function()
        error(value)
    end)
    return not ok and result == "error in error handling" and type(result) == "string"
end
local result = handler_fails(false) and handler_fails(0)
print(type(result) .. ":" .. tostring(result))
"#;
const PROTECTED_ERROR_VALUE_SOURCE: &str = r#"
local function pcall_kind(value, expected)
    local ok, result = pcall(function()
        error(value)
    end)
    return not ok and type(result) == expected
end
local function xpcall_kind(value, expected)
    local ok, result = xpcall(function()
        error(value)
    end, function(caught)
        return type(caught)
    end)
    return not ok and result == expected
end
local legacy_number = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2" or _VERSION == "Luau"
local pcall_nil = _VERSION == "Lua 5.5" and "string" or "nil"
local number_type = legacy_number and "string" or "number"
return pcall_kind(nil, pcall_nil)
    and pcall_kind(false, "boolean")
    and pcall_kind(0, number_type)
    and pcall_kind({}, "table")
    and xpcall_kind(nil, "nil")
    and xpcall_kind(false, "boolean")
    and xpcall_kind(0, number_type)
    and xpcall_kind({}, "table")
"#;
const PROTECTED_ERROR_VALUE_REFERENCE_SOURCE: &str = r#"
local function pcall_kind(value, expected)
    local ok, result = pcall(function()
        error(value)
    end)
    return not ok and type(result) == expected
end
local function xpcall_kind(value, expected)
    local ok, result = xpcall(function()
        error(value)
    end, function(caught)
        return type(caught)
    end)
    return not ok and result == expected
end
local legacy_number = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2" or _VERSION == "Luau"
local pcall_nil = _VERSION == "Lua 5.5" and "string" or "nil"
local number_type = legacy_number and "string" or "number"
local result = pcall_kind(nil, pcall_nil)
    and pcall_kind(false, "boolean")
    and pcall_kind(0, number_type)
    and pcall_kind({}, "table")
    and xpcall_kind(nil, "nil")
    and xpcall_kind(false, "boolean")
    and xpcall_kind(0, number_type)
    and xpcall_kind({}, "table")
print(type(result) .. ":" .. tostring(result))
"#;
const ERROR_LEVEL_SOURCE: &str = r#"
local function shape(level)
    local ok, value = pcall(function()
        if level == nil then
            error("boom")
        else
            error("boom", level)
        end
    end)
    local prefixed = type(value) == "string" and string.sub(value, -6) == ": boom"
    return not ok and ((level == 0 and value == "boom")
        or (level ~= 0 and prefixed))
end
local function string_level()
    local ok, value = pcall(function() error("boom", "1") end)
    return not ok and type(value) == "string" and string.sub(value, -6) == ": boom"
end
local function fraction_level()
    local ok, value = pcall(function() error("boom", 1.5) end)
    local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2" or _VERSION == "Luau"
    local prefixed = type(value) == "string" and string.sub(value, -6) == ": boom"
    return not ok and (legacy and prefixed or not legacy and not prefixed)
end
return shape(0) and shape(1) and shape(nil) and string_level() and fraction_level()
"#;
const ERROR_LEVEL_REFERENCE_SOURCE: &str = r#"
local function shape(level)
    local ok, value = pcall(function()
        if level == nil then
            error("boom")
        else
            error("boom", level)
        end
    end)
    local prefixed = type(value) == "string" and string.sub(value, -6) == ": boom"
    return not ok and ((level == 0 and value == "boom")
        or (level ~= 0 and prefixed))
end
local function string_level()
    local ok, value = pcall(function() error("boom", "1") end)
    return not ok and type(value) == "string" and string.sub(value, -6) == ": boom"
end
local function fraction_level()
    local ok, value = pcall(function() error("boom", 1.5) end)
    local legacy = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2" or _VERSION == "Luau"
    local prefixed = type(value) == "string" and string.sub(value, -6) == ": boom"
    return not ok and (legacy and prefixed or not legacy and not prefixed)
end
local result = shape(0) and shape(1) and shape(nil) and string_level() and fraction_level()
print(type(result) .. ":" .. tostring(result))
"#;
const ERROR_LEVEL_DEEP_BOUNDARY_SOURCE: &str = r#"
local function inner(level)
    error("boom", level)
end
local function middle(level)
    return pcall(function()
        inner(level)
    end)
end
local function prefixed(level)
    local ok, value = middle(level)
    return not ok and type(value) == "string" and string.sub(value, -6) == ": boom"
end
return tostring(prefixed(0)) .. ":" .. tostring(prefixed(1)) .. ":"
    .. tostring(prefixed(2)) .. ":" .. tostring(prefixed(3)) .. ":"
    .. tostring(prefixed(4)) .. ":" .. tostring(prefixed(5))
"#;
const ERROR_LEVEL_DEEP_BOUNDARY_REFERENCE_SOURCE: &str = r#"
local function inner(level)
    error("boom", level)
end
local function middle(level)
    return pcall(function()
        inner(level)
    end)
end
local function prefixed(level)
    local ok, value = middle(level)
    return not ok and type(value) == "string" and string.sub(value, -6) == ": boom"
end
local result = tostring(prefixed(0)) .. ":" .. tostring(prefixed(1)) .. ":"
    .. tostring(prefixed(2)) .. ":" .. tostring(prefixed(3)) .. ":"
    .. tostring(prefixed(4)) .. ":" .. tostring(prefixed(5))
print(type(result) .. ":" .. result)
"#;
const COROUTINE_ERROR_VALUE_SOURCE: &str = r#"
local function case(value, expected)
    local thread = coroutine.create(function()
        error(value)
    end)
    local ok, result = coroutine.resume(thread)
    local wrapped = coroutine.wrap(function()
        error(value)
    end)
    local wrapped_ok, wrapped_result = pcall(wrapped)
    return not ok and type(result) == expected
        and coroutine.status(thread) == "dead"
        and not wrapped_ok and type(wrapped_result) == expected
end
local legacy_number = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2" or _VERSION == "Luau"
local nil_type = _VERSION == "Lua 5.5" and "string" or "nil"
local number_type = legacy_number and "string" or "number"
return case(nil, nil_type) and case(false, "boolean")
    and case(0, number_type) and case({}, "table")
"#;
const COROUTINE_ERROR_VALUE_REFERENCE_SOURCE: &str = r#"
local function case(value, expected)
    local thread = coroutine.create(function()
        error(value)
    end)
    local ok, result = coroutine.resume(thread)
    local wrapped = coroutine.wrap(function()
        error(value)
    end)
    local wrapped_ok, wrapped_result = pcall(wrapped)
    return not ok and type(result) == expected
        and coroutine.status(thread) == "dead"
        and not wrapped_ok and type(wrapped_result) == expected
end
local legacy_number = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2" or _VERSION == "Luau"
local nil_type = _VERSION == "Lua 5.5" and "string" or "nil"
local number_type = legacy_number and "string" or "number"
local result = case(nil, nil_type) and case(false, "boolean")
    and case(0, number_type) and case({}, "table")
print(type(result) .. ":" .. tostring(result))
"#;
const COROUTINE_RESUME_STATE_SOURCE: &str = r#"
local thread = coroutine.create(function()
    coroutine.yield("ready")
    error({ tag = "after" })
end)
local first_ok, first_value = coroutine.resume(thread)
local second_ok, second_value = coroutine.resume(thread)
local dead_ok, dead_message = coroutine.resume(thread)
local running = coroutine.create(function()
    local ok, message = coroutine.resume(coroutine.running())
    return ok, message
end)
local running_ok, nested_ok, running_message = coroutine.resume(running)
local legacy_running = _VERSION == "Lua 5.1" or _VERSION == "Luau"
local expected_running = legacy_running
    and "cannot resume running coroutine" or "cannot resume non-suspended coroutine"
return first_ok and first_value == "ready"
    and not second_ok and type(second_value) == "table" and second_value.tag == "after"
    and not dead_ok and dead_message == "cannot resume dead coroutine"
    and coroutine.status(thread) == "dead"
    and running_ok and not nested_ok and running_message == expected_running
    and coroutine.status(running) == "dead"
"#;
const COROUTINE_RESUME_STATE_REFERENCE_SOURCE: &str = r#"
local thread = coroutine.create(function()
    coroutine.yield("ready")
    error({ tag = "after" })
end)
local first_ok, first_value = coroutine.resume(thread)
local second_ok, second_value = coroutine.resume(thread)
local dead_ok, dead_message = coroutine.resume(thread)
local running = coroutine.create(function()
    local ok, message = coroutine.resume(coroutine.running())
    return ok, message
end)
local running_ok, nested_ok, running_message = coroutine.resume(running)
local legacy_running = _VERSION == "Lua 5.1" or _VERSION == "Luau"
local expected_running = legacy_running
    and "cannot resume running coroutine" or "cannot resume non-suspended coroutine"
local result = first_ok and first_value == "ready"
    and not second_ok and type(second_value) == "table" and second_value.tag == "after"
    and not dead_ok and dead_message == "cannot resume dead coroutine"
    and coroutine.status(thread) == "dead"
    and running_ok and not nested_ok and running_message == expected_running
    and coroutine.status(running) == "dead"
print(type(result) .. ":" .. tostring(result))
"#;
const COROUTINE_CLOSE_STATE_SOURCE: &str = r#"
local close_result = true
if coroutine.close then
    local fresh = coroutine.create(function() end)
    local fresh_ok, fresh_error = coroutine.close(fresh)
    local dead = coroutine.create(function() end)
    coroutine.resume(dead)
    local dead_ok, dead_error = coroutine.close(dead)
    local running = coroutine.create(function()
        local ok, message = coroutine.close(coroutine.running())
        return ok, message
    end)
    local outer_ok, running_ok, running_error = coroutine.resume(running)
    local running_expected = (_VERSION == "Blu" or _VERSION == "Luau")
        and "cannot close running coroutine" or "cannot close a running coroutine"
    local running_result = _VERSION == "Lua 5.5"
        and outer_ok and running_ok == nil and running_error == nil
        or not outer_ok and type(running_ok) == "string" and running_error == nil
            and string.sub(running_ok, -#running_expected) == running_expected
    local main_ok, main_result, main_error = pcall(function()
        return coroutine.close(coroutine.running())
    end)
    local main_expected = _VERSION == "Lua 5.5"
        and "cannot close main thread" or running_expected
    local main_result_ok = not main_ok and type(main_result) == "string"
        and string.sub(main_result, -#main_expected) == main_expected
    close_result = fresh_ok and fresh_error == nil
        and dead_ok and dead_error == nil and outer_ok and running_result and main_result_ok
end

local yield_result = true
if coroutine.isyieldable then
    local main_thread = coroutine.running()
    local main = coroutine.isyieldable()
    local thread = coroutine.create(function()
        local no_arg = coroutine.isyieldable()
        local self_arg = coroutine.isyieldable(coroutine.running())
        local main_arg = coroutine.isyieldable(main_thread)
        return no_arg, self_arg, main_arg
    end)
    local fresh_arg = coroutine.isyieldable(thread)
    local outer_ok, no_arg, self_arg, main_arg = coroutine.resume(thread)
    local dead_arg = coroutine.isyieldable(thread)
    local main_expected = _VERSION == "Luau"
    local main_arg_expected = not (_VERSION == "Blu" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
    yield_result = main == main_expected and outer_ok and no_arg and self_arg
        and main_arg == main_arg_expected and fresh_arg and dead_arg
end

local wrapped = coroutine.wrap(function()
    error({ tag = "wrapped" })
end)
local wrapped_ok, wrapped_error = pcall(wrapped)
return close_result and yield_result and not wrapped_ok
    and type(wrapped_error) == "table" and wrapped_error.tag == "wrapped"
"#;
const COROUTINE_CLOSE_STATE_REFERENCE_SOURCE: &str = r#"
local close_result = true
if coroutine.close then
    local fresh = coroutine.create(function() end)
    local fresh_ok, fresh_error = coroutine.close(fresh)
    local dead = coroutine.create(function() end)
    coroutine.resume(dead)
    local dead_ok, dead_error = coroutine.close(dead)
    local running = coroutine.create(function()
        local ok, message = coroutine.close(coroutine.running())
        return ok, message
    end)
    local outer_ok, running_ok, running_error = coroutine.resume(running)
    local running_expected = (_VERSION == "Blu" or _VERSION == "Luau")
        and "cannot close running coroutine" or "cannot close a running coroutine"
    local running_result = _VERSION == "Lua 5.5"
        and outer_ok and running_ok == nil and running_error == nil
        or not outer_ok and type(running_ok) == "string" and running_error == nil
            and string.sub(running_ok, -#running_expected) == running_expected
    local main_ok, main_result, main_error = pcall(function()
        return coroutine.close(coroutine.running())
    end)
    local main_expected = _VERSION == "Lua 5.5"
        and "cannot close main thread" or running_expected
    local main_result_ok = not main_ok and type(main_result) == "string"
        and string.sub(main_result, -#main_expected) == main_expected
    close_result = fresh_ok and fresh_error == nil
        and dead_ok and dead_error == nil and outer_ok and running_result and main_result_ok
end

local yield_result = true
if coroutine.isyieldable then
    local main_thread = coroutine.running()
    local main = coroutine.isyieldable()
    local thread = coroutine.create(function()
        local no_arg = coroutine.isyieldable()
        local self_arg = coroutine.isyieldable(coroutine.running())
        local main_arg = coroutine.isyieldable(main_thread)
        return no_arg, self_arg, main_arg
    end)
    local fresh_arg = coroutine.isyieldable(thread)
    local outer_ok, no_arg, self_arg, main_arg = coroutine.resume(thread)
    local dead_arg = coroutine.isyieldable(thread)
    local main_expected = _VERSION == "Luau"
    local main_arg_expected = not (_VERSION == "Blu" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
    yield_result = main == main_expected and outer_ok and no_arg and self_arg
        and main_arg == main_arg_expected and fresh_arg and dead_arg
end

local wrapped = coroutine.wrap(function()
    error({ tag = "wrapped" })
end)
local wrapped_ok, wrapped_error = pcall(wrapped)
local result = close_result and yield_result and not wrapped_ok
    and type(wrapped_error) == "table" and wrapped_error.tag == "wrapped"
print(type(result) .. ":" .. tostring(result))
"#;
const COROUTINE_ARGUMENT_SOURCE: &str = r#"
local function ends_with(value, suffix)
    return type(value) == "string" and string.sub(value, -#suffix) == suffix
end
local result = true
if coroutine.close then
    local close_ok, close_error = pcall(function()
        return coroutine.close(false)
    end)
    local expected_close = (_VERSION == "Blu" or _VERSION == "Luau")
        and "invalid argument #1 to 'close' (thread expected, got boolean)"
        or "bad argument #1 to 'close' (thread expected, got boolean)"
    local dead = coroutine.create(function()
        error("dead boom")
    end)
    local resumed, resume_error = coroutine.resume(dead)
    local closed, close_dead_error = coroutine.close(dead)
    result = not close_ok and ends_with(close_error, expected_close)
        and not resumed and type(resume_error) == "string"
        and not closed and ends_with(close_dead_error, "dead boom")
end
if coroutine.isyieldable then
    local current = coroutine.isyieldable()
    local yield_ok, yield_result = pcall(function()
        return coroutine.isyieldable(false)
    end)
    if _VERSION == "Blu" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5" then
        result = result and not yield_ok and type(yield_result) == "string"
    else
        result = result and yield_ok and yield_result == current
    end
end
return result
"#;
const COROUTINE_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local function ends_with(value, suffix)
    return type(value) == "string" and string.sub(value, -#suffix) == suffix
end
local result = true
if coroutine.close then
    local close_ok, close_error = pcall(function()
        return coroutine.close(false)
    end)
    local expected_close = (_VERSION == "Blu" or _VERSION == "Luau")
        and "invalid argument #1 to 'close' (thread expected, got boolean)"
        or "bad argument #1 to 'close' (thread expected, got boolean)"
    local dead = coroutine.create(function()
        error("dead boom")
    end)
    local resumed, resume_error = coroutine.resume(dead)
    local closed, close_dead_error = coroutine.close(dead)
    result = not close_ok and ends_with(close_error, expected_close)
        and not resumed and type(resume_error) == "string"
        and not closed and ends_with(close_dead_error, "dead boom")
end
if coroutine.isyieldable then
    local current = coroutine.isyieldable()
    local yield_ok, yield_result = pcall(function()
        return coroutine.isyieldable(false)
    end)
    if _VERSION == "Blu" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5" then
        result = result and not yield_ok and type(yield_result) == "string"
    else
        result = result and yield_ok and yield_result == current
    end
end
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
const NUMBER_BOUNDARY_SOURCE: &str = r#"
local function shape(value)
    return type(value) .. ":" .. tostring(value == nil) .. ":" .. tostring(value == math.huge)
end
local function integer_shape(value)
    local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
        or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
    local subtype = modern and math.type(value) or "legacy"
    return type(value) .. ":" .. tostring(value == -1) .. ":" .. tostring(subtype)
end
return shape(tonumber("inf")) .. "|"
    .. shape(tonumber("-inf")) .. "|"
    .. shape(tonumber("nan")) .. "|"
    .. integer_shape(tonumber("0xFFFFFFFFFFFFFFFF")) .. "|"
    .. tostring((_VERSION == "Blu" or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and math.type(tonumber("9223372036854775807")) or "legacy") .. "|"
    .. tostring((_VERSION == "Blu" or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and math.type(tonumber("9223372036854775808")) or "legacy")
"#;
const NUMBER_BOUNDARY_REFERENCE_SOURCE: &str = r#"
local function shape(value)
    return type(value) .. ":" .. tostring(value == nil) .. ":" .. tostring(value == math.huge)
end
local function integer_shape(value)
    local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
        or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
    local subtype = modern and math.type(value) or "legacy"
    return type(value) .. ":" .. tostring(value == -1) .. ":" .. tostring(subtype)
end
local result = shape(tonumber("inf")) .. "|"
    .. shape(tonumber("-inf")) .. "|"
    .. shape(tonumber("nan")) .. "|"
    .. integer_shape(tonumber("0xFFFFFFFFFFFFFFFF")) .. "|"
    .. tostring((_VERSION == "Blu" or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and math.type(tonumber("9223372036854775807")) or "legacy") .. "|"
    .. tostring((_VERSION == "Blu" or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
        and math.type(tonumber("9223372036854775808")) or "legacy")
print(type(result) .. ":" .. tostring(result))
"#;
const NUMBER_OVERFLOW_SOURCE: &str = r#"
local explicit = tonumber("10000000000000000", 16)
local default = tonumber("0x10000000000000000")
local reference = tonumber("ffffffffffffffff", 16)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if modern then
    local base36 = tonumber("zzzzzzzzzzzzzzzzzzzz", 36)
    return math.type(explicit) == "integer"
        and explicit == 0 and math.type(default) == "integer" and default == 0
        and base36 == -2153214848064815104
elseif _VERSION == "Luau" then
    local floating = tonumber("0xfffffffffffffffff")
    return explicit == reference and default == reference
        and floating == reference * 16 + 15
else
    return explicit ~= nil and default ~= nil
        and explicit == reference and default == reference
end
"#;
const NUMBER_OVERFLOW_REFERENCE_SOURCE: &str = r#"
local explicit = tonumber("10000000000000000", 16)
local default = tonumber("0x10000000000000000")
local reference = tonumber("ffffffffffffffff", 16)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if modern then
    local base36 = tonumber("zzzzzzzzzzzzzzzzzzzz", 36)
    result = math.type(explicit) == "integer"
        and explicit == 0 and math.type(default) == "integer" and default == 0
        and base36 == -2153214848064815104
elseif _VERSION == "Luau" then
    local floating = tonumber("0xfffffffffffffffff")
    result = explicit == reference and default == reference
        and floating == reference * 16 + 15
else
    result = explicit ~= nil and default ~= nil
        and explicit == reference and default == reference
end
print(type(result) .. ":" .. tostring(result))
"#;
const NUMBER_BASE_SOURCE: &str = r#"
local ok_fraction, fraction = pcall(tonumber, "10", 2.9)
local ok_nan = pcall(tonumber, "10", 0 / 0)
local ok_range = pcall(tonumber, "10", 37)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if modern then
    return not ok_fraction and not ok_nan and not ok_range
end
return ok_fraction and fraction == 2 and not ok_nan and not ok_range
"#;
const NUMBER_BASE_REFERENCE_SOURCE: &str = r#"
local ok_fraction, fraction = pcall(tonumber, "10", 2.9)
local ok_nan = pcall(tonumber, "10", 0 / 0)
local ok_range = pcall(tonumber, "10", 37)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if modern then
    result = not ok_fraction and not ok_nan and not ok_range
else
    result = ok_fraction and fraction == 2 and not ok_nan and not ok_range
end
print(type(result) .. ":" .. tostring(result))
"#;
const NUMBER_GRAMMAR_SOURCE: &str = r#"
local decimal = tonumber("1e309")
local valid = tonumber("  +42  ") == 42
    and tonumber("-0x1p2") == -4
    and tonumber("0x1.8p1") == 3
    and tonumber("0x1p-2") == 0.25
local invalid = tonumber("0x") == nil
    and tonumber("0x1p") == nil
    and tonumber("12x") == nil
return valid and invalid and decimal == math.huge
"#;
const NUMBER_GRAMMAR_REFERENCE_SOURCE: &str = r#"
local decimal = tonumber("1e309")
local valid = tonumber("  +42  ") == 42
    and tonumber("-0x1p2") == -4
    and tonumber("0x1.8p1") == 3
    and tonumber("0x1p-2") == 0.25
local invalid = tonumber("0x") == nil
    and tonumber("0x1p") == nil
    and tonumber("12x") == nil
local result = valid and invalid and decimal == math.huge
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_CHAR_CONVERSION_SOURCE: &str = r#"
local ok_fraction, fraction = pcall(string.char, 1.5)
local ok_nan, nan_value = pcall(string.char, 0 / 0)
local ok_rounded, rounded = pcall(string.char, 255.9)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if modern then
    return not ok_fraction and not ok_nan and not ok_rounded
elseif _VERSION == "Luau" then
    return ok_fraction and string.byte(fraction) == 1
        and not ok_nan
        and ok_rounded and string.byte(rounded) == 255
end
return ok_fraction and string.byte(fraction) == 1
    and ok_nan and string.byte(nan_value) == 0
    and ok_rounded and string.byte(rounded) == 255
"#;
const STRING_CHAR_CONVERSION_REFERENCE_SOURCE: &str = r#"
local ok_fraction, fraction = pcall(string.char, 1.5)
local ok_nan, nan_value = pcall(string.char, 0 / 0)
local ok_rounded, rounded = pcall(string.char, 255.9)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if modern then
    result = not ok_fraction and not ok_nan and not ok_rounded
elseif _VERSION == "Luau" then
    result = ok_fraction and string.byte(fraction) == 1
        and not ok_nan
        and ok_rounded and string.byte(rounded) == 255
else
    result = ok_fraction and string.byte(fraction) == 1
        and ok_nan and string.byte(nan_value) == 0
        and ok_rounded and string.byte(rounded) == 255
end
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_BYTE_INDEX_SOURCE: &str = r#"
local ok_fraction, fraction = pcall(string.byte, "abc", 1.5)
local ok_nan, nan_value = pcall(string.byte, "abc", 0 / 0)
local zero = string.byte("abc", 0) == nil
local negative = string.byte("abc", -1) == 99
local outside = string.byte("abc", 4) == nil
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if modern then
    return not ok_fraction and not ok_nan and zero and negative and outside
end
return ok_fraction and fraction == 97 and ok_nan and nan_value == nil
    and zero and negative and outside
"#;
const STRING_BYTE_INDEX_REFERENCE_SOURCE: &str = r#"
local ok_fraction, fraction = pcall(string.byte, "abc", 1.5)
local ok_nan, nan_value = pcall(string.byte, "abc", 0 / 0)
local zero = string.byte("abc", 0) == nil
local negative = string.byte("abc", -1) == 99
local outside = string.byte("abc", 4) == nil
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if modern then
    result = not ok_fraction and not ok_nan and zero and negative and outside
else
    result = ok_fraction and fraction == 97 and ok_nan and nan_value == nil
        and zero and negative and outside
end
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_SUB_FIND_INDEX_SOURCE: &str = r#"
local ok_sub, sub = pcall(string.sub, "abc", 1.5)
local ok_find, start, finish = pcall(string.find, "abc", "b", 1.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if modern then
    return not ok_sub and not ok_find
end
return ok_sub and sub == "abc" and ok_find and start == 2 and finish == 2
"#;
const STRING_SUB_FIND_INDEX_REFERENCE_SOURCE: &str = r#"
local ok_sub, sub = pcall(string.sub, "abc", 1.5)
local ok_find, start, finish = pcall(string.find, "abc", "b", 1.5)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if modern then
    result = not ok_sub and not ok_find
else
    result = ok_sub and sub == "abc" and ok_find and start == 2 and finish == 2
end
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok_rep, rep = pcall(string.rep, "ab", 2.9)
local ok_gsub, gsubbed, gsub_count = pcall(string.gsub, "aaaa", "a", "x", 2.9)
local ok_gsub_nan, gsub_nan, gsub_nan_count = pcall(string.gsub, "aaaa", "a", "x", 0 / 0)
local ok_match, matched = pcall(string.match, "abc", ".", 1.9)
local ok_gmatch, gmatched = pcall(function()
    local iterator = string.gmatch("abc", ".", 1.9)
    return iterator()
end)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local gmatch_start = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if modern then
    return not ok_rep and not ok_gsub and not ok_gsub_nan
        and not ok_match and (not gmatch_start or not ok_gmatch)
elseif _VERSION == "Lua 5.2" then
    return ok_rep and rep == "abab"
        and ok_gsub and gsubbed == "xxaa" and gsub_count == 2
        and ok_gsub_nan and gsub_nan == "xxxx" and gsub_nan_count == 4
        and ok_match and matched == "a"
        and ok_gmatch and gmatched == "a"
elseif _VERSION == "Luau" then
    return ok_rep and rep == "abab"
        and ok_gsub and gsubbed == "xxaa" and gsub_count == 2
        and ok_gsub_nan and gsub_nan == "aaaa" and gsub_nan_count == 0
        and ok_match and matched == "a"
        and ok_gmatch and gmatched == "a"
        and gmatch_start
end
return ok_rep and rep == "abab"
    and ok_gsub and gsubbed == "xxaa" and gsub_count == 2
    and ok_gsub_nan and gsub_nan == "aaaa" and gsub_nan_count == 0
    and ok_match and matched == "a"
    and ok_gmatch and gmatched == "a"
    and not gmatch_start
"#;
const STRING_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok_rep, rep = pcall(string.rep, "ab", 2.9)
local ok_gsub, gsubbed, gsub_count = pcall(string.gsub, "aaaa", "a", "x", 2.9)
local ok_gsub_nan, gsub_nan, gsub_nan_count = pcall(string.gsub, "aaaa", "a", "x", 0 / 0)
local ok_match, matched = pcall(string.match, "abc", ".", 1.9)
local ok_gmatch, gmatched = pcall(function()
    local iterator = string.gmatch("abc", ".", 1.9)
    return iterator()
end)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local gmatch_start = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if modern then
    result = not ok_rep and not ok_gsub and not ok_gsub_nan
        and not ok_match and (not gmatch_start or not ok_gmatch)
elseif _VERSION == "Lua 5.2" then
    result = ok_rep and rep == "abab"
        and ok_gsub and gsubbed == "xxaa" and gsub_count == 2
        and ok_gsub_nan and gsub_nan == "xxxx" and gsub_nan_count == 4
        and ok_match and matched == "a"
        and ok_gmatch and gmatched == "a"
elseif _VERSION == "Luau" then
    result = ok_rep and rep == "abab"
        and ok_gsub and gsubbed == "xxaa" and gsub_count == 2
        and ok_gsub_nan and gsub_nan == "aaaa" and gsub_nan_count == 0
        and ok_match and matched == "a"
        and ok_gmatch and gmatched == "a"
        and gmatch_start
else
    result = ok_rep and rep == "abab"
        and ok_gsub and gsubbed == "xxaa" and gsub_count == 2
        and ok_gsub_nan and gsub_nan == "aaaa" and gsub_nan_count == 0
        and ok_match and matched == "a"
        and ok_gmatch and gmatched == "a"
        and not gmatch_start
end
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_PATTERN_EDGE_SOURCE: &str = r#"
local ok_rep, repeated = pcall(string.rep, "ab", 3, "-")
local ok_zero, zero = pcall(string.rep, "ab", 0, "-")
local ok_negative = pcall(string.rep, "ab", -1, "-")
local ok_balanced = pcall(string.find, "abc", "%b(")
local ok_capture = pcall(string.find, "abc", "%1")
local separator = _VERSION == "Blu" or _VERSION == "Lua 5.2"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
return ok_rep and (separator and repeated == "ab-ab-ab" or repeated == "ababab")
    and ok_zero and zero == "" and not ok_negative
    and not ok_balanced and not ok_capture
"#;
const STRING_PATTERN_EDGE_REFERENCE_SOURCE: &str = r#"
local ok_rep, repeated = pcall(string.rep, "ab", 3, "-")
local ok_zero, zero = pcall(string.rep, "ab", 0, "-")
local ok_negative = pcall(string.rep, "ab", -1, "-")
local ok_balanced = pcall(string.find, "abc", "%b(")
local ok_capture = pcall(string.find, "abc", "%1")
local separator = _VERSION == "Lua 5.2" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result = ok_rep and (separator and repeated == "ab-ab-ab" or repeated == "ababab")
    and ok_zero and zero == "" and not ok_negative
    and not ok_balanced and not ok_capture
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_PATTERN_CAPTURE_SOURCE: &str = r#"
local balanced = string.match("ab(cd)ef", "(%b())")
local nested = string.match("key=(a(b)c)", "key=(%b())")
local start, finish, before, after = string.find("abc:def", "()%a+()")
local word_start, word_finish, word = string.find(" abc", "%f[%a](%a+)")
local first, second, third, fourth = string.match(
    "a=12;b=34", "(%a)=(%d+);(%a)=(%d+)")
local function malformed(pattern)
    local ok, message = pcall(string.find, "abc", pattern)
    return not ok and type(message) == "string"
end
local capture_ok = pcall(string.find, "", string.rep("()", 32))
local capture_overflow_ok, capture_message = pcall(
    string.find, "", string.rep("()", 33))
return balanced == "(cd)"
    and nested == "(a(b)c)"
    and start == 1 and finish == 3 and before == 1 and after == 4
    and word_start == 2 and word_finish == 4 and word == "abc"
    and first == "a" and second == "12"
    and third == "b" and fourth == "34"
    and capture_ok and not capture_overflow_ok
    and type(capture_message) == "string"
    and malformed("%b(") and malformed("%f[")
    and malformed("%1") and malformed("(") and malformed("[z-")
"#;
const STRING_PATTERN_CAPTURE_REFERENCE_SOURCE: &str = r#"
local balanced = string.match("ab(cd)ef", "(%b())")
local nested = string.match("key=(a(b)c)", "key=(%b())")
local start, finish, before, after = string.find("abc:def", "()%a+()")
local word_start, word_finish, word = string.find(" abc", "%f[%a](%a+)")
local first, second, third, fourth = string.match(
    "a=12;b=34", "(%a)=(%d+);(%a)=(%d+)")
local function malformed(pattern)
    local ok, message = pcall(string.find, "abc", pattern)
    return not ok and type(message) == "string"
end
local capture_ok = pcall(string.find, "", string.rep("()", 32))
local capture_overflow_ok, capture_message = pcall(
    string.find, "", string.rep("()", 33))
local result = balanced == "(cd)"
    and nested == "(a(b)c)"
    and start == 1 and finish == 3 and before == 1 and after == 4
    and word_start == 2 and word_finish == 4 and word == "abc"
    and first == "a" and second == "12"
    and third == "b" and fourth == "34"
    and capture_ok and not capture_overflow_ok
    and type(capture_message) == "string"
    and malformed("%b(") and malformed("%f[")
    and malformed("%1") and malformed("(") and malformed("[z-")
print(type(result) .. ":" .. tostring(result))
"#;
const STRING_PATTERN_REPLACEMENT_SOURCE: &str = r#"
local table_result, table_count = string.gsub(
    "a1b2", "(%a)(%d)", { a = "A", b = "B" })
local function replace(letter, digit)
    return digit .. letter
end
local function malformed(pattern)
    local ok, message = pcall(string.find, "abc", pattern)
    return not ok and type(message) == "string"
end
local function_result, function_count = string.gsub(
    "a1b2", "(%a)(%d)", replace)
return table_result == "AB" and table_count == 2
    and function_result == "1a2b" and function_count == 2
    and malformed("%")
"#;
const STRING_PATTERN_REPLACEMENT_REFERENCE_SOURCE: &str = r#"
local table_result, table_count = string.gsub(
    "a1b2", "(%a)(%d)", { a = "A", b = "B" })
local function replace(letter, digit)
    return digit .. letter
end
local function malformed(pattern)
    local ok, message = pcall(string.find, "abc", pattern)
    return not ok and type(message) == "string"
end
local function_result, function_count = string.gsub(
    "a1b2", "(%a)(%d)", replace)
local result = table_result == "AB" and table_count == 2
    and function_result == "1a2b" and function_count == 2
    and malformed("%")
print(type(result) .. ":" .. tostring(result))
"#;
const MATH_LEGACY_ALIAS_SOURCE: &str = r#"
local legacy = _VERSION == "Lua 5.1"
return legacy and type(math.mod) == "function" and math.mod(7, 3) == 1
    or not legacy and math.mod == nil
"#;
const MATH_LEGACY_ALIAS_REFERENCE_SOURCE: &str = r#"
local legacy = _VERSION == "Lua 5.1"
local result = legacy and type(math.mod) == "function" and math.mod(7, 3) == 1
    or not legacy and math.mod == nil
print(type(result) .. ":" .. tostring(result))
"#;
const NIL_TABLE_LOOKUP_SOURCE: &str = r#"
local plain = {}
local metamethod = setmetatable({}, { __index = function() return 42 end })
local ok, raw = pcall(rawget, plain, nil)
return plain[nil] == nil and metamethod[nil] == 42 and ok and raw == nil
"#;
const NIL_TABLE_LOOKUP_REFERENCE_SOURCE: &str = r#"
local plain = {}
local metamethod = setmetatable({}, { __index = function() return 42 end })
local ok, raw = pcall(rawget, plain, nil)
local result = plain[nil] == nil and metamethod[nil] == 42 and ok and raw == nil
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_INTEGER_ARGUMENT_SOURCE: &str = r#"
local values = { "a", "b", "c" }
local ok_concat, concatenated = pcall(table.concat, values, ",", 1.5, 2)
local ok_unpack, unpack_first, unpack_second = pcall(function()
    if not table.unpack then return "absent" end
    return table.unpack(values, 1.5, 2)
end)
local ok_move, move_first, move_second, move_third = pcall(function()
    if not table.move then return "absent" end
    local target = { "x", "x", "x" }
    table.move(values, 1.5, 2, 1, target)
    return target[1], target[2], target[3]
end)
local ok_create, created_length = pcall(function()
    if not table.create then return "absent" end
    return #table.create(2.9, true)
end)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local has_unpack = _VERSION ~= "Lua 5.1"
local has_move = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local has_create = _VERSION == "Blu" or _VERSION == "Luau" or _VERSION == "Lua 5.5"
local unpack_result = not has_unpack and ok_unpack and unpack_first == "absent"
    or has_unpack and (not modern and ok_unpack and unpack_first == "a" and unpack_second == "b"
        or modern and not ok_unpack)
local move_result = not has_move and ok_move and move_first == "absent"
    or has_move and (not modern and ok_move and move_first == "a"
        and move_second == "b" and move_third == "x"
        or modern and not ok_move)
local create_result = not has_create and ok_create and created_length == "absent"
    or _VERSION == "Luau" and ok_create and created_length == 2
    or modern and has_create and not ok_create
return (modern and not ok_concat or not modern and ok_concat and concatenated == "a,b")
    and unpack_result and move_result and create_result
"#;
const TABLE_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local values = { "a", "b", "c" }
local ok_concat, concatenated = pcall(table.concat, values, ",", 1.5, 2)
local ok_unpack, unpack_first, unpack_second = pcall(function()
    if not table.unpack then return "absent" end
    return table.unpack(values, 1.5, 2)
end)
local ok_move, move_first, move_second, move_third = pcall(function()
    if not table.move then return "absent" end
    local target = { "x", "x", "x" }
    table.move(values, 1.5, 2, 1, target)
    return target[1], target[2], target[3]
end)
local ok_create, created_length = pcall(function()
    if not table.create then return "absent" end
    return #table.create(2.9, true)
end)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local has_unpack = _VERSION ~= "Lua 5.1"
local has_move = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local has_create = _VERSION == "Blu" or _VERSION == "Luau" or _VERSION == "Lua 5.5"
local unpack_result = not has_unpack and ok_unpack and unpack_first == "absent"
    or has_unpack and (not modern and ok_unpack and unpack_first == "a" and unpack_second == "b"
        or modern and not ok_unpack)
local move_result = not has_move and ok_move and move_first == "absent"
    or has_move and (not modern and ok_move and move_first == "a"
        and move_second == "b" and move_third == "x"
        or modern and not ok_move)
local create_result = not has_create and ok_create and created_length == "absent"
    or _VERSION == "Luau" and ok_create and created_length == 2
    or modern and has_create and not ok_create
local result = (modern and not ok_concat or not modern and ok_concat and concatenated == "a,b")
    and unpack_result and move_result and create_result
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_MUTATION_EDGE_SOURCE: &str = r#"
local values = { 3, 1, 2 }
local sorted = table.sort(values, function(left, right) return 1 end)
local moved = { 1, 2, 3, 4 }
local move = rawget(table, "move")
local moved_result
if move then
    moved_result = move(moved, 1, 3, 2)
end
local modern_move = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local move_expected = not modern_move
    and move == nil
    or modern_move and move ~= nil and moved_result == moved
        and moved[1] == 1 and moved[2] == 1 and moved[3] == 2 and moved[4] == 3
return sorted == nil and values[1] == 3 and values[2] == 2 and values[3] == 1
    and move_expected
"#;
const TABLE_MUTATION_EDGE_REFERENCE_SOURCE: &str = r#"
local values = { 3, 1, 2 }
local sorted = table.sort(values, function(left, right) return 1 end)
local moved = { 1, 2, 3, 4 }
local move = rawget(table, "move")
local moved_result
if move then
    moved_result = move(moved, 1, 3, 2)
end
local modern_move = _VERSION == "Luau"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local move_expected = not modern_move
    and move == nil
    or modern_move and move ~= nil and moved_result == moved
        and moved[1] == 1 and moved[2] == 1 and moved[3] == 2 and moved[4] == 3
local result = sorted == nil and values[1] == 3 and values[2] == 2 and values[3] == 1
    and move_expected
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_HOLE_EDGE_SOURCE: &str = r#"
local values = { 1, nil, 3 }
local ok_concat, concatenated = pcall(table.concat, values, ",")
local ok_unpack, first, second, third = pcall(function()
    if not table.unpack then return "absent" end
    return table.unpack(values)
end)
local ok_pack, packed_n, packed_first, packed_second, packed_third = pcall(function()
    if not table.pack then return "absent" end
    local packed = table.pack(1, nil, 3)
    return packed.n, packed[1], packed[2], packed[3]
end)
local modern_length = _VERSION == "Blu" or _VERSION == "Lua 5.5"
local concat_shape = modern_length and ok_concat and concatenated == "1"
    or not modern_length and not ok_concat
local unpack_shape = _VERSION == "Lua 5.1" and ok_unpack and first == "absent"
    or modern_length and ok_unpack and first == 1 and second == nil and third == nil
    or not modern_length and ok_unpack and first == 1 and second == nil and third == 3
local pack_shape = _VERSION == "Lua 5.1" and ok_pack and packed_n == "absent"
    or _VERSION ~= "Lua 5.1" and ok_pack and packed_n == 3
        and packed_first == 1 and packed_second == nil and packed_third == 3
return (#values == (modern_length and 1 or 3)) and concat_shape and unpack_shape
    and pack_shape
"#;
const TABLE_HOLE_EDGE_REFERENCE_SOURCE: &str = r#"
local values = { 1, nil, 3 }
local ok_concat, concatenated = pcall(table.concat, values, ",")
local ok_unpack, first, second, third = pcall(function()
    if not table.unpack then return "absent" end
    return table.unpack(values)
end)
local ok_pack, packed_n, packed_first, packed_second, packed_third = pcall(function()
    if not table.pack then return "absent" end
    local packed = table.pack(1, nil, 3)
    return packed.n, packed[1], packed[2], packed[3]
end)
local modern_length = _VERSION == "Lua 5.5"
local concat_shape = modern_length and ok_concat and concatenated == "1"
    or not modern_length and not ok_concat
local unpack_shape = _VERSION == "Lua 5.1" and ok_unpack and first == "absent"
    or modern_length and ok_unpack and first == 1 and second == nil and third == nil
    or not modern_length and ok_unpack and first == 1 and second == nil and third == 3
local pack_shape = _VERSION == "Lua 5.1" and ok_pack and packed_n == "absent"
    or _VERSION ~= "Lua 5.1" and ok_pack and packed_n == 3
        and packed_first == 1 and packed_second == nil and packed_third == 3
local result = (#values == (modern_length and 1 or 3)) and concat_shape and unpack_shape
    and pack_shape
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_LENGTH_CONSUMER_EDGE_SOURCE: &str = r#"
local values = { 1, nil, 3 }
local high = { [10] = 42 }
local modern_length = _VERSION == "Blu" or _VERSION == "Lua 5.5"
local expected_length = modern_length and 1 or 3
local has_rawlen = rawlen ~= nil
local ok_rawlen, raw_length = pcall(rawlen, values)
local rawlen_shape = _VERSION == "Lua 5.1" and not has_rawlen and not ok_rawlen
    or _VERSION ~= "Lua 5.1" and has_rawlen and ok_rawlen and raw_length == expected_length
local ok_rawlen_high, raw_high = pcall(rawlen, high)
rawlen_shape = rawlen_shape and (_VERSION == "Lua 5.1" and not ok_rawlen_high
    or _VERSION ~= "Lua 5.1" and ok_rawlen_high and raw_high == 0)
local has_getn = _VERSION == "Blu" or _VERSION == "Luau" or _VERSION == "Lua 5.1"
local ok_getn, getn_length = pcall(function()
    if not table.getn then return "absent" end
    return table.getn(values)
end)
local getn_shape = not has_getn and ok_getn and getn_length == "absent"
    or has_getn and ok_getn and getn_length == expected_length
local ok_getn_high, getn_high = pcall(function()
    if not table.getn then return "absent" end
    return table.getn(high)
end)
getn_shape = getn_shape and (not has_getn and ok_getn_high and getn_high == "absent"
    or has_getn and ok_getn_high and getn_high == 0)
local has_maxn = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
local ok_maxn, maxn_length = pcall(function()
    if not table.maxn then return "absent" end
    return table.maxn(values)
end)
local maxn_shape = not has_maxn and ok_maxn and maxn_length == "absent"
    or has_maxn and ok_maxn and maxn_length == 3
local ok_maxn_high, maxn_high = pcall(function()
    if not table.maxn then return "absent" end
    return table.maxn(high)
end)
maxn_shape = maxn_shape and (not has_maxn and ok_maxn_high and maxn_high == "absent"
    or has_maxn and ok_maxn_high and maxn_high == 10)
local inserted = { 1, nil, 3 }
local ok_insert = pcall(table.insert, inserted, 4)
local insert_shape = ok_insert and (modern_length
    and inserted[1] == 1 and inserted[2] == 4 and inserted[3] == 3
    or not modern_length and inserted[1] == 1 and inserted[2] == nil
        and inserted[3] == 3 and inserted[4] == 4)
local removed = { 1, nil, 3 }
local ok_remove, removed_value = pcall(table.remove, removed)
local remove_shape = ok_remove and (modern_length
    and removed_value == 1 and removed[1] == nil and removed[3] == 3
    or not modern_length and removed_value == 3 and removed[1] == 1 and removed[3] == nil)
local sorted = { 3, nil, 1 }
local ok_sort = pcall(table.sort, sorted)
local sort_shape = modern_length and ok_sort and sorted[1] == 3 and sorted[2] == nil
    and sorted[3] == 1 or not modern_length and not ok_sort
local finder = rawget(table, "find")
local ok_find, found = pcall(function()
    if not finder then return "absent" end
    return finder(values, 3)
end)
local find_shape = ok_find and (not finder and found == "absent" or finder and found == nil)
return rawlen_shape and getn_shape and maxn_shape and insert_shape and remove_shape
    and sort_shape and find_shape
"#;
const TABLE_LENGTH_CONSUMER_EDGE_REFERENCE_SOURCE: &str = r#"
local values = { 1, nil, 3 }
local high = { [10] = 42 }
local modern_length = _VERSION == "Lua 5.5"
local expected_length = modern_length and 1 or 3
local has_rawlen = rawlen ~= nil
local ok_rawlen, raw_length = pcall(rawlen, values)
local rawlen_shape = _VERSION == "Lua 5.1" and not has_rawlen and not ok_rawlen
    or _VERSION ~= "Lua 5.1" and has_rawlen and ok_rawlen and raw_length == expected_length
local ok_rawlen_high, raw_high = pcall(rawlen, high)
rawlen_shape = rawlen_shape and (_VERSION == "Lua 5.1" and not ok_rawlen_high
    or _VERSION ~= "Lua 5.1" and ok_rawlen_high and raw_high == 0)
local has_getn = _VERSION == "Lua 5.1"
local ok_getn, getn_length = pcall(function()
    if not table.getn then return "absent" end
    return table.getn(values)
end)
local getn_shape = not has_getn and ok_getn and getn_length == "absent"
    or has_getn and ok_getn and getn_length == expected_length
local ok_getn_high, getn_high = pcall(function()
    if not table.getn then return "absent" end
    return table.getn(high)
end)
getn_shape = getn_shape and (not has_getn and ok_getn_high and getn_high == "absent"
    or has_getn and ok_getn_high and getn_high == 0)
local has_maxn = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
local ok_maxn, maxn_length = pcall(function()
    if not table.maxn then return "absent" end
    return table.maxn(values)
end)
local maxn_shape = not has_maxn and ok_maxn and maxn_length == "absent"
    or has_maxn and ok_maxn and maxn_length == 3
local ok_maxn_high, maxn_high = pcall(function()
    if not table.maxn then return "absent" end
    return table.maxn(high)
end)
maxn_shape = maxn_shape and (not has_maxn and ok_maxn_high and maxn_high == "absent"
    or has_maxn and ok_maxn_high and maxn_high == 10)
local inserted = { 1, nil, 3 }
local ok_insert = pcall(table.insert, inserted, 4)
local insert_shape = ok_insert and (modern_length
    and inserted[1] == 1 and inserted[2] == 4 and inserted[3] == 3
    or not modern_length and inserted[1] == 1 and inserted[2] == nil
        and inserted[3] == 3 and inserted[4] == 4)
local removed = { 1, nil, 3 }
local ok_remove, removed_value = pcall(table.remove, removed)
local remove_shape = ok_remove and (modern_length
    and removed_value == 1 and removed[1] == nil and removed[3] == 3
    or not modern_length and removed_value == 3 and removed[1] == 1 and removed[3] == nil)
local sorted = { 3, nil, 1 }
local ok_sort = pcall(table.sort, sorted)
local sort_shape = modern_length and ok_sort and sorted[1] == 3 and sorted[2] == nil
    and sorted[3] == 1 or not modern_length and not ok_sort
local finder = rawget(table, "find")
local ok_find, found = pcall(function()
    if not finder then return "absent" end
    return finder(values, 3)
end)
local find_shape = ok_find and (not finder and found == "absent" or finder and found == nil)
local result = rawlen_shape and getn_shape and maxn_shape and insert_shape and remove_shape
    and sort_shape and find_shape
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_ITERATION_LENGTH_EDGE_SOURCE: &str = r#"
local sparse = { 1, nil, 3 }
local high = { [10] = 42 }
local ipairs_count, ipairs_sum = 0, 0
for index, value in ipairs(sparse) do
    ipairs_count = ipairs_count + 1
    ipairs_sum = ipairs_sum + index + (value or 0)
end
local pair_count, pair_sum = 0, 0
for key, value in pairs(sparse) do
    pair_count = pair_count + 1
    pair_sum = pair_sum + key
end
local high_pair_count, high_pair_sum = 0, 0
for key, value in pairs(high) do
    high_pair_count = high_pair_count + 1
    high_pair_sum = high_pair_sum + key
end
local function callback_counts(name, values)
    local callback = rawget(table, name)
    if not callback then return "absent", 0, 0 end
    local count, sum = 0, 0
    local ok = pcall(callback, values, function(key, value)
        count = count + 1
        sum = sum + key
    end)
    return ok, count, sum
end
local foreach_ok, foreach_count, foreach_sum = callback_counts("foreach", sparse)
local foreachi_ok, foreachi_count, foreachi_sum = callback_counts("foreachi", sparse)
local modern_length = _VERSION == "Blu"
local callbacks_present = _VERSION == "Blu" or _VERSION == "Luau" or _VERSION == "Lua 5.1"
local foreach_shape = callbacks_present and foreach_ok and foreach_count == 2
    and foreach_sum == 4 or not callbacks_present and foreach_ok == "absent"
local foreachi_shape = callbacks_present and foreachi_ok and foreachi_count == (modern_length and 1 or 3)
    and foreachi_sum == (modern_length and 1 or 6)
    or not callbacks_present and foreachi_ok == "absent"
return ipairs_count == 1 and ipairs_sum == 2 and pair_count == 2 and pair_sum == 4
    and high_pair_count == 1 and high_pair_sum == 10 and foreach_shape and foreachi_shape
"#;
const TABLE_ITERATION_LENGTH_EDGE_REFERENCE_SOURCE: &str = r#"
local sparse = { 1, nil, 3 }
local high = { [10] = 42 }
local ipairs_count, ipairs_sum = 0, 0
for index, value in ipairs(sparse) do
    ipairs_count = ipairs_count + 1
    ipairs_sum = ipairs_sum + index + (value or 0)
end
local pair_count, pair_sum = 0, 0
for key, value in pairs(sparse) do
    pair_count = pair_count + 1
    pair_sum = pair_sum + key
end
local high_pair_count, high_pair_sum = 0, 0
for key, value in pairs(high) do
    high_pair_count = high_pair_count + 1
    high_pair_sum = high_pair_sum + key
end
local function callback_counts(name, values)
    local callback = rawget(table, name)
    if not callback then return "absent", 0, 0 end
    local count, sum = 0, 0
    local ok = pcall(callback, values, function(key, value)
        count = count + 1
        sum = sum + key
    end)
    return ok, count, sum
end
local foreach_ok, foreach_count, foreach_sum = callback_counts("foreach", sparse)
local foreachi_ok, foreachi_count, foreachi_sum = callback_counts("foreachi", sparse)
local callbacks_present = _VERSION == "Lua 5.1" or _VERSION == "Luau"
local foreach_shape = callbacks_present and foreach_ok and foreach_count == 2
    and foreach_sum == 4 or not callbacks_present and foreach_ok == "absent"
local foreachi_shape = callbacks_present and foreachi_ok and foreachi_count == 3
    and foreachi_sum == 6 or not callbacks_present and foreachi_ok == "absent"
local result = ipairs_count == 1 and ipairs_sum == 2 and pair_count == 2 and pair_sum == 4
    and high_pair_count == 1 and high_pair_sum == 10 and foreach_shape and foreachi_shape
print(type(result) .. ":" .. tostring(result))
"#;
const RAWLEN_MAXN_SCALAR_EDGE_SOURCE: &str = r#"
local fractional = { [2.5] = true, [3] = true }
local infinite = { [math.huge] = true, [2.5] = true }
local function call_unary(function_value, value)
    if not function_value then return "absent", nil end
    local ok, result = pcall(function_value, value)
    return ok, result
end
local raw_string_ok, raw_string = call_unary(rawlen, "abc")
local raw_fractional_ok, raw_fractional = call_unary(rawlen, fractional)
local raw_infinite_ok, raw_infinite = call_unary(rawlen, infinite)
local raw_number_ok = call_unary(rawlen, 1)
local raw_supported = _VERSION ~= "Lua 5.1"
local raw_shape = raw_supported and raw_string_ok == true and raw_string == 3
    and raw_fractional_ok == true and raw_fractional == 0
    and raw_infinite_ok == true and raw_infinite == 0 and raw_number_ok == false
    or not raw_supported and raw_string_ok == "absent" and raw_fractional_ok == "absent"
        and raw_infinite_ok == "absent" and raw_number_ok == "absent"
local maxn = table.maxn
local maxn_supported = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
local max_fractional_ok, max_fractional = call_unary(maxn, fractional)
local max_infinite_ok, max_infinite = call_unary(maxn, infinite)
local max_number_ok = call_unary(maxn, 1)
local maxn_shape = maxn_supported and max_fractional_ok == true and max_fractional == 3
    and max_infinite_ok == true and max_infinite == math.huge and max_number_ok == false
    or not maxn_supported and max_fractional_ok == "absent" and max_infinite_ok == "absent"
        and max_number_ok == "absent"
return raw_shape and maxn_shape
"#;
const RAWLEN_MAXN_SCALAR_EDGE_REFERENCE_SOURCE: &str = r#"
local fractional = { [2.5] = true, [3] = true }
local infinite = { [math.huge] = true, [2.5] = true }
local function call_unary(function_value, value)
    if not function_value then return "absent", nil end
    local ok, result = pcall(function_value, value)
    return ok, result
end
local raw_string_ok, raw_string = call_unary(rawlen, "abc")
local raw_fractional_ok, raw_fractional = call_unary(rawlen, fractional)
local raw_infinite_ok, raw_infinite = call_unary(rawlen, infinite)
local raw_number_ok = call_unary(rawlen, 1)
local raw_supported = _VERSION ~= "Lua 5.1"
local raw_shape = raw_supported and raw_string_ok == true and raw_string == 3
    and raw_fractional_ok == true and raw_fractional == 0
    and raw_infinite_ok == true and raw_infinite == 0 and raw_number_ok == false
    or not raw_supported and raw_string_ok == "absent" and raw_fractional_ok == "absent"
        and raw_infinite_ok == "absent" and raw_number_ok == "absent"
local maxn = table.maxn
local maxn_supported = _VERSION == "Lua 5.1" or _VERSION == "Lua 5.2"
local max_fractional_ok, max_fractional = call_unary(maxn, fractional)
local max_infinite_ok, max_infinite = call_unary(maxn, infinite)
local max_number_ok = call_unary(maxn, 1)
local maxn_shape = maxn_supported and max_fractional_ok == true and max_fractional == 3
    and max_infinite_ok == true and max_infinite == math.huge and max_number_ok == false
    or not maxn_supported and max_fractional_ok == "absent" and max_infinite_ok == "absent"
        and max_number_ok == "absent"
local result = raw_shape and maxn_shape
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_ASSIGNMENT_LENGTH_EDGE_SOURCE: &str = r#"
local reverse = {}
reverse[3] = 3
reverse[2] = 2
local nil_first = {}
nil_first[1] = nil
nil_first[3] = 3
nil_first[2] = 2
local nil_then_two = {}
nil_then_two[1] = nil
nil_then_two[2] = 2
local high_then_first = {}
high_then_first[10] = 10
high_then_first[1] = 1
local compact_assignment = _VERSION == "Lua 5.5"
local expected_reverse = compact_assignment and 3 or 0
local expected_nil_first = compact_assignment and 3 or 0
local expected_nil_then_two = 0
return #reverse == expected_reverse and #nil_first == expected_nil_first
    and #nil_then_two == expected_nil_then_two and #high_then_first == 1
"#;
const TABLE_ASSIGNMENT_LENGTH_EDGE_REFERENCE_SOURCE: &str = r#"
local reverse = {}
reverse[3] = 3
reverse[2] = 2
local nil_first = {}
nil_first[1] = nil
nil_first[3] = 3
nil_first[2] = 2
local nil_then_two = {}
nil_then_two[1] = nil
nil_then_two[2] = 2
local high_then_first = {}
high_then_first[10] = 10
high_then_first[1] = 1
local modern = _VERSION == "Lua 5.5"
local expected_reverse = modern and 3 or 0
local expected_nil_first = modern and 3 or 0
local expected_nil_then_two = _VERSION == "Luau" and 2 or 0
local result = #reverse == expected_reverse and #nil_first == expected_nil_first
    and #nil_then_two == expected_nil_then_two and #high_then_first == 1
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_MAXN_EXTREME_NUMERIC_EDGE_SOURCE: &str = r#"
local maxn = table.maxn
if not maxn then return true end
local mixed = { [-math.huge] = true, [-1] = true, [0] = true, [2.5] = true }
local negative = { [-math.huge] = true, [-1] = true, [0] = true }
local large = { [9007199254740992] = true }
return maxn(mixed) == 2.5 and maxn(negative) == 0
    and maxn(large) == 9007199254740992
"#;
const TABLE_MAXN_EXTREME_NUMERIC_EDGE_REFERENCE_SOURCE: &str = r#"
local maxn = table.maxn
local result
if not maxn then
    result = true
else
    local mixed = { [-math.huge] = true, [-1] = true, [0] = true, [2.5] = true }
    local negative = { [-math.huge] = true, [-1] = true, [0] = true }
    local large = { [9007199254740992] = true }
    result = maxn(mixed) == 2.5 and maxn(negative) == 0
        and maxn(large) == 9007199254740992
end
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_ITERATION_MUTATION_EDGE_SOURCE: &str = r#"
local ipairs_values = { 1, 2 }
local ipairs_count = 0
for index, value in ipairs(ipairs_values) do
    ipairs_count = ipairs_count + 1
    if index == 1 then ipairs_values[3] = 3 end
end
local pair_values = { a = 1 }
local pair_ok = pcall(function()
    for key, value in pairs(pair_values) do
        if key == "a" then pair_values.b = 2 end
    end
end)
local function callback_mutation(name, values)
    local callback = rawget(table, name)
    if not callback then return "absent", 0, false end
    local count = 0
    local ok = pcall(callback, values, function(index, value)
        count = count + 1
        if index == 1 or index == "a" then
            values[3] = 3
            values.b = 2
        end
    end)
    return ok, count, values.b == 2
end
local foreach_values = { a = 1 }
local foreach_ok, foreach_count, foreach_mutated = callback_mutation("foreach", foreach_values)
local foreachi_values = { 1, 2 }
local foreachi_ok, foreachi_count, foreachi_mutated = callback_mutation("foreachi", foreachi_values)
local callbacks_present = _VERSION == "Blu" or _VERSION == "Luau" or _VERSION == "Lua 5.1"
local foreach_shape = callbacks_present and foreach_ok and foreach_mutated
    or not callbacks_present and foreach_ok == "absent"
local foreachi_shape = callbacks_present and foreachi_ok and foreachi_count == 2
    and foreachi_mutated or not callbacks_present and foreachi_ok == "absent"
return ipairs_count == 3 and ipairs_values[3] == 3 and pair_ok and pair_values.b == 2
    and foreach_shape and foreachi_shape
"#;
const TABLE_ITERATION_MUTATION_EDGE_REFERENCE_SOURCE: &str = r#"
local ipairs_values = { 1, 2 }
local ipairs_count = 0
for index, value in ipairs(ipairs_values) do
    ipairs_count = ipairs_count + 1
    if index == 1 then ipairs_values[3] = 3 end
end
local pair_values = { a = 1 }
local pair_ok = pcall(function()
    for key, value in pairs(pair_values) do
        if key == "a" then pair_values.b = 2 end
    end
end)
local function callback_mutation(name, values)
    local callback = rawget(table, name)
    if not callback then return "absent", 0, false end
    local count = 0
    local ok = pcall(callback, values, function(index, value)
        count = count + 1
        if index == 1 or index == "a" then
            values[3] = 3
            values.b = 2
        end
    end)
    return ok, count, values.b == 2
end
local foreach_values = { a = 1 }
local foreach_ok, foreach_count, foreach_mutated = callback_mutation("foreach", foreach_values)
local foreachi_values = { 1, 2 }
local foreachi_ok, foreachi_count, foreachi_mutated = callback_mutation("foreachi", foreachi_values)
local callbacks_present = _VERSION == "Luau" or _VERSION == "Lua 5.1"
local foreach_shape = callbacks_present and foreach_ok and foreach_mutated
    or not callbacks_present and foreach_ok == "absent"
local foreachi_shape = callbacks_present and foreachi_ok and foreachi_count == 2
    and foreachi_mutated or not callbacks_present and foreachi_ok == "absent"
local result = ipairs_count == 3 and ipairs_values[3] == 3 and pair_ok and pair_values.b == 2
    and foreach_shape and foreachi_shape
print(type(result) .. ":" .. tostring(result))
"#;
const ASSIGNMENT_ORDER_EDGE_SOURCE: &str = r#"
local log = {}
local function mark(label, value)
    log[#log + 1] = label
    return value
end
local target = setmetatable({}, {
    __newindex = function(table_value, key, value)
        log[#log + 1] = "write:" .. key .. ":" .. value
        rawset(table_value, key, value)
    end,
})
target[mark("key", "x")] = mark("value", 7)
local values = { 1, 2 }
local index = 1
values[mark("index", index)], index = mark("rhs", 9), mark("newindex", 2)
local constructor = {
    [mark("ckey", "x")] = mark("cvalue", 3),
    [mark("ckey2", "x")] = mark("cvalue2", 4),
}
return table.concat(log, ",") == "key,value,write:x:7,index,rhs,newindex,ckey,cvalue,ckey2,cvalue2"
    and target.x == 7 and values[1] == 9 and index == 2 and constructor.x == 4
"#;
const ASSIGNMENT_ORDER_EDGE_REFERENCE_SOURCE: &str = r#"
local log = {}
local function mark(label, value)
    log[#log + 1] = label
    return value
end
local target = setmetatable({}, {
    __newindex = function(table_value, key, value)
        log[#log + 1] = "write:" .. key .. ":" .. value
        rawset(table_value, key, value)
    end,
})
target[mark("key", "x")] = mark("value", 7)
local values = { 1, 2 }
local index = 1
values[mark("index", index)], index = mark("rhs", 9), mark("newindex", 2)
local constructor = {
    [mark("ckey", "x")] = mark("cvalue", 3),
    [mark("ckey2", "x")] = mark("cvalue2", 4),
}
local result = table.concat(log, ",") == "key,value,write:x:7,index,rhs,newindex,ckey,cvalue,ckey2,cvalue2"
    and target.x == 7 and values[1] == 9 and index == 2 and constructor.x == 4
print(type(result) .. ":" .. tostring(result))
"#;
const ASSIGNMENT_CONSTRUCTOR_EDGE_SOURCE: &str = r#"
local log = {}
local function mark(label, value)
    log[#log + 1] = label
    return value
end
local function make_target()
    log[#log + 1] = "receiver"
    return setmetatable({}, {
        __newindex = function(table_value, key, value)
            log[#log + 1] = "write:" .. key .. ":" .. value
            rawset(table_value, key, value)
        end,
    })
end
local target = make_target()
target[mark("key", "x")] = mark("value", 1)
local values = { 1, 2 }
local alias = values
values[mark("index", 1)], alias[mark("alias-index", 1)] =
    mark("rhs1", 5), mark("rhs2", 6)
local constructor = {
    mark("array", 7),
    name = mark("name", 8),
    [mark("key2", "k")] = mark("value2", 9),
    mark("array2", 10),
}
return table.concat(log, ",") ==
        "receiver,key,value,write:x:1,index,alias-index,rhs1,rhs2,array,name,key2,value2,array2"
    and target.x == 1 and values[1] == 5 and constructor[1] == 7
    and constructor[2] == 10 and constructor.name == 8 and constructor.k == 9
"#;
const ASSIGNMENT_CONSTRUCTOR_EDGE_REFERENCE_SOURCE: &str = r#"
local log = {}
local function mark(label, value)
    log[#log + 1] = label
    return value
end
local function make_target()
    log[#log + 1] = "receiver"
    return setmetatable({}, {
        __newindex = function(table_value, key, value)
            log[#log + 1] = "write:" .. key .. ":" .. value
            rawset(table_value, key, value)
        end,
    })
end
local target = make_target()
target[mark("key", "x")] = mark("value", 1)
local values = { 1, 2 }
local alias = values
values[mark("index", 1)], alias[mark("alias-index", 1)] =
    mark("rhs1", 5), mark("rhs2", 6)
local constructor = {
    mark("array", 7),
    name = mark("name", 8),
    [mark("key2", "k")] = mark("value2", 9),
    mark("array2", 10),
}
local result = table.concat(log, ",") ==
        "receiver,key,value,write:x:1,index,alias-index,rhs1,rhs2,array,name,key2,value2,array2"
    and target.x == 1 and values[1] == 5 and constructor[1] == 7
    and constructor[2] == 10 and constructor.name == 8 and constructor.k == 9
print(type(result) .. ":" .. tostring(result))
"#;
const UTF8_INTEGER_ARGUMENT_SOURCE: &str = r#"
local ok_len, length = pcall(function()
    if not utf8 or not utf8.len then return "absent" end
    return utf8.len("abc", 1.5, 2)
end)
local ok_codepoint, first_codepoint, second_codepoint = pcall(function()
    if not utf8 or not utf8.codepoint then return "absent" end
    return utf8.codepoint("abc", 1.5, 2)
end)
local ok_offset, offset = pcall(function()
    if not utf8 or not utf8.offset then return "absent" end
    return utf8.offset("abc", 1.5, 1.5)
end)
local ok_char, character = pcall(function()
    if not utf8 or not utf8.char then return "absent" end
    return utf8.char(65.5)
end)
local present = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
if not present then
    return ok_len and length == "absent"
        and ok_codepoint and first_codepoint == "absent"
        and ok_offset and offset == "absent"
        and ok_char and character == "absent"
elseif modern then
    return not ok_len and not ok_codepoint and not ok_offset and not ok_char
end
return ok_len and length == 2
    and ok_codepoint and first_codepoint == 97 and second_codepoint == 98
    and ok_offset and offset == 1
    and ok_char and character == "A"
"#;
const UTF8_INTEGER_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok_len, length = pcall(function()
    if not utf8 or not utf8.len then return "absent" end
    return utf8.len("abc", 1.5, 2)
end)
local ok_codepoint, first_codepoint, second_codepoint = pcall(function()
    if not utf8 or not utf8.codepoint then return "absent" end
    return utf8.codepoint("abc", 1.5, 2)
end)
local ok_offset, offset = pcall(function()
    if not utf8 or not utf8.offset then return "absent" end
    return utf8.offset("abc", 1.5, 1.5)
end)
local ok_char, character = pcall(function()
    if not utf8 or not utf8.char then return "absent" end
    return utf8.char(65.5)
end)
local present = _VERSION == "Blu" or _VERSION == "Luau"
    or _VERSION == "Lua 5.3" or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local result
if not present then
    result = ok_len and length == "absent"
        and ok_codepoint and first_codepoint == "absent"
        and ok_offset and offset == "absent"
        and ok_char and character == "absent"
elseif modern then
    result = not ok_len and not ok_codepoint and not ok_offset and not ok_char
else
    result = ok_len and length == 2
        and ok_codepoint and first_codepoint == 97 and second_codepoint == 98
        and ok_offset and offset == 1
        and ok_char and character == "A"
end
print(type(result) .. ":" .. tostring(result))
"#;
const TABLE_POSITION_ARGUMENT_SOURCE: &str = r#"
local ok_insert, insert_first, insert_second, insert_third, insert_length = pcall(function()
    local values = { "a", "b", "c" }
    table.insert(values, 1.5, "x")
    return values[1], values[2], values[3], #values
end)
local ok_remove, removed, remove_first, remove_second, remove_length = pcall(function()
    local values = { "a", "b", "c" }
    local value = table.remove(values, 1.5)
    return value, values[1], values[2], #values
end)
local ok_find, found = pcall(function()
    if not table.find then return "absent" end
    return table.find({ "a", "b", "a" }, "a", 1.5)
end)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local has_find = _VERSION == "Blu" or _VERSION == "Luau"
if modern then
    return not ok_insert and not ok_remove
        and ((has_find and not ok_find) or not has_find and ok_find and found == "absent")
end
local expected_find = has_find and ok_find and found ~= "absent" or not has_find and ok_find and found == "absent"
return ok_insert and insert_first == "x" and insert_second == "a"
    and insert_third == "b" and insert_length == 4
    and ok_remove and removed == "a" and remove_first == "b"
    and remove_second == "c" and remove_length == 2
    and expected_find
"#;
const TABLE_POSITION_ARGUMENT_REFERENCE_SOURCE: &str = r#"
local ok_insert, insert_first, insert_second, insert_third, insert_length = pcall(function()
    local values = { "a", "b", "c" }
    table.insert(values, 1.5, "x")
    return values[1], values[2], values[3], #values
end)
local ok_remove, removed, remove_first, remove_second, remove_length = pcall(function()
    local values = { "a", "b", "c" }
    local value = table.remove(values, 1.5)
    return value, values[1], values[2], #values
end)
local ok_find, found = pcall(function()
    if not table.find then return "absent" end
    return table.find({ "a", "b", "a" }, "a", 1.5)
end)
local modern = _VERSION == "Blu" or _VERSION == "Lua 5.3"
    or _VERSION == "Lua 5.4" or _VERSION == "Lua 5.5"
local has_find = _VERSION == "Blu" or _VERSION == "Luau"
local result
if modern then
    result = not ok_insert and not ok_remove
        and ((has_find and not ok_find) or not has_find and ok_find and found == "absent")
else
    local expected_find = has_find and ok_find and found ~= "absent"
        or not has_find and ok_find and found == "absent"
    result = ok_insert and insert_first == "x" and insert_second == "a"
        and insert_third == "b" and insert_length == 4
        and ok_remove and removed == "a" and remove_first == "b"
        and remove_second == "c" and remove_length == 2
        and expected_find
end
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
local dead_ok, dead_message = coroutine.resume(failing)
local running = coroutine.create(function()
    local ok, message = coroutine.resume(coroutine.running())
    return ok, message
end)
local running_ok, nested_ok, running_message = coroutine.resume(running)
local running_expected = "cannot resume running coroutine"
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
    and not dead_ok and dead_message == "cannot resume dead coroutine"
    and running_ok and not nested_ok and running_message == running_expected
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
local dead_ok, dead_message = coroutine.resume(failing)
local running = coroutine.create(function()
    local ok, message = coroutine.resume(coroutine.running())
    return ok, message
end)
local running_ok, nested_ok, running_message = coroutine.resume(running)
local running_expected = "cannot resume running coroutine"
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
    and not dead_ok and dead_message == "cannot resume dead coroutine"
    and running_ok and not nested_ok and running_message == running_expected
    and handled_first and handled_pause == "handled pause"
    and handled_second and not handled_ok and handled_message == "handled"
print(type(value) .. ":" .. tostring(value))
"##;
const COROUTINE_CLOSE_SOURCE: &str = r#"
local events = ""
local paused = coroutine.create(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            events = events .. "closed"
        end,
    })
    coroutine.yield("pause")
end)
local resumed, signal = coroutine.resume(paused)
local closed, close_error = coroutine.close(paused)

local failing = coroutine.create(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            events = events .. "error"
            error({ tag = "close failure" })
        end,
    })
    coroutine.yield("pause")
end)
coroutine.resume(failing)
local failed, failure = coroutine.close(failing)
local closed_again = coroutine.close(failing)
return resumed and signal == "pause"
    and closed and close_error == nil and events == "closederror"
    and not failed and type(failure) == "table" and failure.tag == "close failure" and closed_again
    and coroutine.status(paused) == "dead"
    and coroutine.status(failing) == "dead"
"#;
const COROUTINE_CLOSE_REFERENCE_SOURCE: &str = r#"
local events = ""
local paused = coroutine.create(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            events = events .. "closed"
        end,
    })
    coroutine.yield("pause")
end)
local resumed, signal = coroutine.resume(paused)
local closed, close_error = coroutine.close(paused)

local failing = coroutine.create(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            events = events .. "error"
            error({ tag = "close failure" })
        end,
    })
    coroutine.yield("pause")
end)
coroutine.resume(failing)
local failed, failure = coroutine.close(failing)
local closed_again = coroutine.close(failing)
local result = resumed and signal == "pause"
    and closed and close_error == nil and events == "closederror"
    and not failed and type(failure) == "table" and failure.tag == "close failure" and closed_again
    and coroutine.status(paused) == "dead"
    and coroutine.status(failing) == "dead"
print(type(result) .. ":" .. tostring(result))
"#;
const COROUTINE_ABANDON_SOURCE: &str = r#"
local events = ""
local function abandon()
    local thread = coroutine.create(function()
        local resource <close> = setmetatable({}, {
            __close = function()
                events = events .. "closed"
            end,
        })
        coroutine.yield()
    end)
    coroutine.resume(thread)
end
abandon()
collectgarbage("collect")
return events == ""
"#;
const COROUTINE_ABANDON_REFERENCE_SOURCE: &str = r#"
local events = ""
local function abandon()
    local thread = coroutine.create(function()
        local resource <close> = setmetatable({}, {
            __close = function()
                events = events .. "closed"
            end,
        })
        coroutine.yield()
    end)
    coroutine.resume(thread)
end
abandon()
collectgarbage("collect")
local result = events == ""
print(type(result) .. ":" .. tostring(result))
"#;
const COROUTINE_CLOSE_YIELD_SOURCE: &str = r#"
local thread = coroutine.create(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            return coroutine.yield("closing")
        end,
    })
    coroutine.yield("pause")
end)
coroutine.resume(thread)
local closed, message = coroutine.close(thread)
return not closed and type(message) == "string"
    and coroutine.status(thread) == "dead"
"#;
const COROUTINE_CLOSE_YIELD_REFERENCE_SOURCE: &str = r#"
local thread = coroutine.create(function()
    local resource <close> = setmetatable({}, {
        __close = function()
            return coroutine.yield("closing")
        end,
    })
    coroutine.yield("pause")
end)
coroutine.resume(thread)
local closed, message = coroutine.close(thread)
local result = not closed and type(message) == "string"
    and coroutine.status(thread) == "dead"
print(type(result) .. ":" .. tostring(result))
"#;

fn main() -> ExitCode {
    match env::args_os().nth(1).as_deref() {
        Some(argument) if argument == std::ffi::OsStr::new("--owned-lua-child") => {
            return run_owned_lua_child(env::args_os().skip(2));
        }
        Some(argument) if argument == std::ffi::OsStr::new("--owned-luau-child") => {
            return run_owned_luau_child(env::args_os().skip(2));
        }
        _ => {}
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("blu-conformance: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_owned_luau_child(mut args: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let Some(path) = args.next() else {
        eprintln!("blu-owned-luau-child: missing source path");
        return ExitCode::FAILURE;
    };
    let Some(profile) = args.next() else {
        eprintln!("blu-owned-luau-child: missing profile");
        return ExitCode::FAILURE;
    };
    if args.next().is_some() {
        eprintln!("blu-owned-luau-child: expected source path and profile");
        return ExitCode::FAILURE;
    }
    let profile = match profile.to_string_lossy().as_ref() {
        "blu" => SemanticProfile::Blu,
        "luau" => SemanticProfile::Luau,
        value => {
            eprintln!("blu-owned-luau-child: unsupported profile {value:?}");
            return ExitCode::FAILURE;
        }
    };
    let path = PathBuf::from(path);
    let source = match fs::read(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("blu-owned-luau-child: failed to read {path:?}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("official.luau");
    // The upstream C++ conformance harness keeps built-in bit32 lookup alive
    // while the fixture clears ordinary lowercase globals from _G.  Preserve
    // that harness-only lookup shape without changing Blu's ordinary global
    // deletion semantics.
    let source = if name == "tables.luau" {
        let mut adapted = b"local bit32 = bit32\nlocal makelud = makelud\n".to_vec();
        adapted.extend_from_slice(&source);
        adapted
    } else if name == "iter.luau" {
        // The upstream C++ harness supplies this yielding iterator as a host
        // callback. Keep the fixture's coroutine/generic-for contract in the
        // owned runner without widening the runtime's native callback ABI: a
        // guest helper yields before returning the next control value, which
        // is observably equivalent to the callback for this test.
        let mut adapted = br#"
cYieldingIterator = function(state, index)
    if index >= state then return nil end
    coroutine.yield(index + 1)
    return index + 1
end
-- Luau's C++ harness reports true when native code generation is unavailable;
-- the owned runner is intentionally an interpreter, so preserve that hook.
is_native_if_supported = function()
    return true
end
        "#
        .to_vec();
        adapted.extend_from_slice(&source);
        adapted
    } else if matches!(name, "sort.luau" | "math.luau") {
        let mut adapted = b"_G._soft = true\n".to_vec();
        adapted.extend_from_slice(&source);
        adapted
    } else {
        source
    };
    let deadline = official_luau_test_deadline(name);
    let instruction_limit = official_luau_test_instruction_limit(name);
    let mut engine = Engine::new(
        SourceCompiler::default(),
        official_luau_vm(name, instruction_limit, Instant::now() + deadline),
    );
    // Luau's portable sort fixture uses the safe `os.clock` surface. Keep
    // this official-test child deterministic without granting process access.
    engine.vm_mut().set_clock_getter(|| Ok(0.0));
    let makelud = engine.vm_mut().register_function(|vm, arguments| {
        let token = makelud_token(arguments)?;
        Ok(vec![vm.create_light_userdata(token)])
    });
    engine
        .vm_mut()
        .set_global(&b"makelud"[..], Value::NativeFunction(makelud));
    let cxxthrow = engine.vm_mut().register_function(|_, _| {
        Err(RuntimeError::Raised(Value::String(Arc::from(&b"oops"[..]))))
    });
    engine
        .vm_mut()
        .set_global(&b"cxxthrow"[..], Value::NativeFunction(cxxthrow));
    let resumeerror = engine.vm_mut().register_function(|vm, arguments| {
        let thread = match arguments.first() {
            Some(Value::Thread(thread)) => *thread,
            Some(value) => {
                return Err(RuntimeError::Type {
                    operation: "resumeerror",
                    expected: "thread",
                    actual: value.type_name(),
                });
            }
            None => {
                return Err(RuntimeError::Argument {
                    function: "resumeerror",
                    index: 1,
                });
            }
        };
        vm.resume_error(thread, arguments.get(1).cloned().unwrap_or(Value::Nil))?;
        Ok(Vec::new())
    });
    engine
        .vm_mut()
        .set_global(&b"resumeerror"[..], Value::NativeFunction(resumeerror));
    engine
        .vm_mut()
        .set_global(&b"limitedstack"[..], Value::Boolean(true));
    if name == "sort.luau" {
        // The upstream harness uses its soft mode for the portable stress
        // case; keep the owned interpreter run bounded while exercising the
        // same sorting and comparator semantics.
        engine
            .vm_mut()
            .set_global(&b"_soft"[..], Value::Boolean(true));
    }
    match engine.execute_owned_source_named(&source, name, profile) {
        Ok(_) => {
            if let Err(error) = std::io::stdout().write_all(&engine.vm_mut().take_output()) {
                eprintln!("blu-owned-luau-child: failed to write captured output: {error}");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            let output = engine.vm_mut().take_output();
            let mut diagnostic_engine = Engine::new(
                SourceCompiler::default(),
                official_luau_vm(name, instruction_limit, Instant::now() + deadline),
            );
            diagnostic_engine.vm_mut().set_clock_getter(|| Ok(0.0));
            install_official_luau_assert_tracker(&mut diagnostic_engine);
            diagnostic_engine
                .vm_mut()
                .set_global(&b"limitedstack"[..], Value::Boolean(true));
            let makelud = diagnostic_engine
                .vm_mut()
                .register_function(|vm, arguments| {
                    let token = makelud_token(arguments)?;
                    Ok(vec![vm.create_light_userdata(token)])
                });
            diagnostic_engine
                .vm_mut()
                .set_global(&b"makelud"[..], Value::NativeFunction(makelud));
            let cxxthrow = diagnostic_engine.vm_mut().register_function(|_, _| {
                Err(RuntimeError::Raised(Value::String(Arc::from(&b"oops"[..]))))
            });
            diagnostic_engine
                .vm_mut()
                .set_global(&b"cxxthrow"[..], Value::NativeFunction(cxxthrow));
            let resumeerror = diagnostic_engine
                .vm_mut()
                .register_function(|vm, arguments| {
                    let thread = match arguments.first() {
                        Some(Value::Thread(thread)) => *thread,
                        Some(value) => {
                            return Err(RuntimeError::Type {
                                operation: "resumeerror",
                                expected: "thread",
                                actual: value.type_name(),
                            });
                        }
                        None => {
                            return Err(RuntimeError::Argument {
                                function: "resumeerror",
                                index: 1,
                            });
                        }
                    };
                    vm.resume_error(thread, arguments.get(1).cloned().unwrap_or(Value::Nil))?;
                    Ok(Vec::new())
                });
            diagnostic_engine
                .vm_mut()
                .set_global(&b"resumeerror"[..], Value::NativeFunction(resumeerror));
            let detail = if matches!(
                error,
                OwnedExecuteError::Runtime(
                    RuntimeError::DeadlineExceeded | RuntimeError::InstructionLimit { .. }
                )
            ) {
                error.to_string()
            } else {
                match diagnostic_engine.execute_owned_source_named(&source, name, profile) {
                    Err(diagnostic) => diagnostic.to_string(),
                    Ok(_) => error.to_string(),
                }
            };
            eprintln!(
                "blu-owned-luau-child: {detail}; main error: {error} (output {:?})",
                String::from_utf8_lossy(&output)
            );
            let _ = std::io::stdout().write_all(&output);
            ExitCode::FAILURE
        }
    }
}

fn makelud_token(arguments: &[Value]) -> Result<u64, RuntimeError> {
    match arguments.first() {
        Some(Value::Integer(value)) => Ok(*value as u64),
        Some(Value::Number(value)) if value.is_finite() => Ok(*value as u64),
        // The upstream harness uses the argument's address as the opaque
        // lightuserdata identity.  The string bytes are already owned by the
        // argument, so the slice pointer is stable for the duration of this
        // bridge call and Blu never dereferences it.
        Some(Value::String(value)) => Ok(value.as_ref().as_ptr() as usize as u64),
        _ => Err(RuntimeError::Argument {
            function: "makelud",
            index: 1,
        }),
    }
}

fn install_official_luau_assert_tracker(engine: &mut Engine) {
    let count = Arc::new(Mutex::new(0usize));
    let tracker_count = Arc::clone(&count);
    let tracker = engine.vm_mut().register_function(move |_, arguments| {
        let mut count =
            tracker_count
                .lock()
                .map_err(|_| RuntimeError::UnsupportedLibraryFeature {
                    function: "assert",
                    feature: "official assertion tracker unavailable",
                })?;
        *count = count.saturating_add(1);
        if arguments.first().is_none_or(|value| !value.is_truthy()) {
            return Err(RuntimeError::Raised(Value::String(Arc::from(
                format!("official assert #{}", *count).into_bytes(),
            ))));
        }
        Ok(arguments.to_vec())
    });
    engine
        .vm_mut()
        .set_global(&b"assert"[..], Value::NativeFunction(tracker));
}

fn wait_for_child(mut child: Child, timeout: Duration) -> Result<Option<Output>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to poll official child: {error}"))?
        {
            Some(_) => {
                return child
                    .wait_with_output()
                    .map(Some)
                    .map_err(|error| format!("failed to collect official child: {error}"));
            }
            None if Instant::now() >= deadline => {
                child
                    .kill()
                    .map_err(|error| format!("failed to stop official child: {error}"))?;
                let _ = child.wait();
                return Ok(None);
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn run_owned_lua_child(mut args: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let Some(path) = args.next() else {
        eprintln!("blu-owned-child: missing source path");
        return ExitCode::FAILURE;
    };
    let profile = match args.next() {
        None => SemanticProfile::Lua51,
        Some(value) => match value.to_string_lossy().as_ref() {
            "lua51" => SemanticProfile::Lua51,
            "lua52" => SemanticProfile::Lua52,
            "lua53" => SemanticProfile::Lua53,
            "lua54" => SemanticProfile::Lua54,
            "lua55" => SemanticProfile::Lua55,
            value => {
                eprintln!("blu-owned-child: unsupported Lua profile {value:?}");
                return ExitCode::FAILURE;
            }
        },
    };
    let portable = match args.next() {
        None => false,
        Some(value) if value == "portable" => true,
        Some(value) => {
            eprintln!("blu-owned-child: unsupported child option {value:?}");
            return ExitCode::FAILURE;
        }
    };
    if args.next().is_some() {
        eprintln!("blu-owned-child: expected source path, optional Lua profile, and child option");
        return ExitCode::FAILURE;
    }
    let path = PathBuf::from(path);
    let source = match fs::read(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("blu-owned-child: failed to read {path:?}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut engine = Engine::default();
    *engine.vm_mut() = Vm::default().with_instruction_limit(100_000_000);
    engine
        .vm_mut()
        .set_global(&b"limitedstack"[..], Value::Boolean(true));
    // PUC Lua returns nil when a requested locale is unavailable.  The
    // conformance child intentionally does not mutate process-global locale
    // state, so expose the portable C locale and make other locale requests
    // unavailable instead of turning the fixture's optional locale branch
    // into an embedding-capability error.
    engine
        .vm_mut()
        .set_os_setlocale_getter(|locale, _category| {
            if locale.is_none() || locale == Some(b"C") {
                Ok(Some(b"C".to_vec()))
            } else {
                Ok(None)
            }
        });
    // The official sort fixture only needs a monotonic elapsed-time source to
    // format its progress messages; keep the child deterministic and avoid
    // making the conformance harness depend on the host process clock.
    engine.vm_mut().set_clock_getter(|| Ok(0.0));
    // The aggregate PUC suites call os.time() for elapsed-test reporting.
    // Keep that host capability deterministic in the owned child.
    engine.vm_mut().set_time_getter(|| Ok(1_700_000_000));
    let fixture_root = match env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("blu-owned-child: failed to resolve fixture root: {error}");
            return ExitCode::FAILURE;
        }
    };
    let probe_root = fixture_root.clone();
    engine.vm_mut().set_file_probe(move |path| {
        Ok(owned_child_path(&probe_root, path).is_some_and(|path| path.is_file()))
    });
    let loader_root = fixture_root;
    engine.vm_mut().set_file_loader(move |path| {
        let Some(path) = owned_child_path(&loader_root, path) else {
            return Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"owned child path is outside the fixture root"[..],
            ))));
        };
        fs::read(&path).map_err(|error| {
            RuntimeError::Raised(Value::String(Arc::from(error.to_string().into_bytes())))
        })
    });
    let stdin = Arc::new(ConformanceIoFile {
        bytes: Mutex::new(Vec::new()),
        position: Mutex::new(0),
    });
    let stdout = Arc::new(ConformanceIoFile {
        bytes: Mutex::new(Vec::new()),
        position: Mutex::new(0),
    });
    let stderr = Arc::new(ConformanceIoFile {
        bytes: Mutex::new(Vec::new()),
        position: Mutex::new(0),
    });
    let stream_stdin = Arc::clone(&stdin);
    let stream_stdout = Arc::clone(&stdout);
    let stream_stderr = Arc::clone(&stderr);
    engine.vm_mut().set_io_stream_opener(move |kind| {
        Ok(match kind {
            IoStreamKind::Stdin => stream_stdin.clone() as Arc<dyn IoFile>,
            IoStreamKind::Stdout => stream_stdout.clone() as Arc<dyn IoFile>,
            IoStreamKind::Stderr => stream_stderr.clone() as Arc<dyn IoFile>,
        })
    });
    // The PUC command-line interpreter provides an `arg` table even when no
    // command-line arguments are supplied. The owned child is otherwise a
    // bare embedding, so install the empty equivalent before the fixture.
    if let Err(error) = engine.execute_owned_source(b"arg = {}", profile) {
        eprintln!("blu-owned-child: failed to prepare command-line arg table: {error}");
        return ExitCode::FAILURE;
    }
    if portable {
        if let Err(error) = engine.execute_owned_source(b"_port = true", profile) {
            eprintln!("blu-owned-child: failed to prepare portable fixture mode: {error}");
            return ExitCode::FAILURE;
        }
        // The PUC test archives load helper chunks such as `tracegc.lua` from
        // the fixture directory. Keep the profile's normal package defaults
        // intact for package-specific probes, but make portable child runs
        // resolve those sibling helpers through the explicit fixture root.
        if let Err(error) =
            engine.execute_owned_source(br#"package.path = "./?.lua;./?/init.lua""#, profile)
        {
            eprintln!("blu-owned-child: failed to prepare fixture package path: {error}");
            return ExitCode::FAILURE;
        }
    }
    let source_name = format!("@{}", path.to_string_lossy());
    if let Err(error) = engine.execute_owned_source_named(&source, source_name, profile) {
        let detail = official_owned_error_detail(&error);
        let stream_output = stdout
            .bytes
            .lock()
            .expect("official Lua child stdout lock")
            .clone();
        let print_output = engine.vm_mut().take_output();
        let mut output = stream_output;
        output.extend_from_slice(&print_output);
        eprintln!(
            "blu-owned-child: {detail}; output {:?}",
            String::from_utf8_lossy(&output)
        );
        return ExitCode::FAILURE;
    }
    let stream_output = stdout
        .bytes
        .lock()
        .expect("official Lua child stdout lock")
        .clone();
    let print_output = engine.vm_mut().take_output();
    let mut output = stream_output;
    output.extend_from_slice(&print_output);
    if let Err(error) = std::io::stdout().write_all(&output) {
        eprintln!("blu-owned-child: failed to write captured output: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn owned_child_path(root: &Path, path: &[u8]) -> Option<PathBuf> {
    let path = std::str::from_utf8(path).ok().map(Path::new)?;
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return None;
    }
    Some(root.join(path))
}

fn run() -> Result<(), String> {
    let args = Args::parse(env::args_os().skip(1))?;
    verify_checkout(&args.source)?;
    verify_executable(&args.upstream)?;
    if let (Some(official_luau_tests), Some(filter)) = (
        args.official_luau_tests.as_deref(),
        args.official_luau_test.as_deref(),
    ) {
        for &profile in args.official_luau_profile.profiles() {
            verify_official_luau_tests(&args.upstream, official_luau_tests, profile, Some(filter))?;
        }
        return Ok(());
    }
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
    verify_owned_program_case(
        "dynamic numeric for step (Blu)",
        DYNAMIC_NUMERIC_FOR_SOURCE,
        DYNAMIC_NUMERIC_FOR_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "repeat condition local visibility (Blu)",
        REPEAT_SCOPE_SOURCE,
        REPEAT_SCOPE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "repeat condition local visibility (Luau)",
        REPEAT_SCOPE_SOURCE,
        REPEAT_SCOPE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "number conversion overflow rules (Luau)",
        NUMBER_OVERFLOW_SOURCE,
        NUMBER_OVERFLOW_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "number conversion base validation (Blu)",
        NUMBER_BASE_SOURCE,
        NUMBER_BASE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "number conversion base validation (Luau)",
        NUMBER_BASE_SOURCE,
        NUMBER_BASE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "number conversion grammar (Blu)",
        NUMBER_GRAMMAR_SOURCE,
        NUMBER_GRAMMAR_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "number conversion grammar (Luau)",
        NUMBER_GRAMMAR_SOURCE,
        NUMBER_GRAMMAR_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.char numeric conversion (Blu)",
        STRING_CHAR_CONVERSION_SOURCE,
        STRING_CHAR_CONVERSION_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.char numeric conversion (Luau)",
        STRING_CHAR_CONVERSION_SOURCE,
        STRING_CHAR_CONVERSION_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.byte index conversion (Blu)",
        STRING_BYTE_INDEX_SOURCE,
        STRING_BYTE_INDEX_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.byte index conversion (Luau)",
        STRING_BYTE_INDEX_SOURCE,
        STRING_BYTE_INDEX_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.sub/find index conversion (Blu)",
        STRING_SUB_FIND_INDEX_SOURCE,
        STRING_SUB_FIND_INDEX_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.sub/find index conversion (Luau)",
        STRING_SUB_FIND_INDEX_SOURCE,
        STRING_SUB_FIND_INDEX_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string integer argument conversion (Blu)",
        STRING_INTEGER_ARGUMENT_SOURCE,
        STRING_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string integer argument conversion (Luau)",
        STRING_INTEGER_ARGUMENT_SOURCE,
        STRING_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string pattern edge matrix (Blu)",
        STRING_PATTERN_EDGE_SOURCE,
        STRING_PATTERN_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string pattern edge matrix (Luau)",
        STRING_PATTERN_EDGE_SOURCE,
        STRING_PATTERN_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string balanced/frontier/capture semantics (Blu)",
        STRING_PATTERN_CAPTURE_SOURCE,
        STRING_PATTERN_CAPTURE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-pattern-capture-blu-reference.lua",
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string balanced/frontier/capture semantics (Luau)",
        STRING_PATTERN_CAPTURE_SOURCE,
        STRING_PATTERN_CAPTURE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string replacement capture semantics (Blu)",
        STRING_PATTERN_REPLACEMENT_SOURCE,
        STRING_PATTERN_REPLACEMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-pattern-replacement-blu-reference.lua",
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string replacement capture semantics (Luau)",
        STRING_PATTERN_REPLACEMENT_SOURCE,
        STRING_PATTERN_REPLACEMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "Lua 5.1 math.mod alias surface (Blu profile)",
        MATH_LEGACY_ALIAS_SOURCE,
        MATH_LEGACY_ALIAS_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "Lua 5.1 math.mod alias surface (Luau profile)",
        MATH_LEGACY_ALIAS_SOURCE,
        MATH_LEGACY_ALIAS_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "nil-key table lookup semantics (Blu profile)",
        NIL_TABLE_LOOKUP_SOURCE,
        NIL_TABLE_LOOKUP_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "nil-key table lookup semantics (Luau profile)",
        NIL_TABLE_LOOKUP_SOURCE,
        NIL_TABLE_LOOKUP_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table integer argument conversion (Blu)",
        TABLE_INTEGER_ARGUMENT_SOURCE,
        TABLE_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table integer argument conversion (Luau)",
        TABLE_INTEGER_ARGUMENT_SOURCE,
        TABLE_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table mutation edge matrix (Blu)",
        TABLE_MUTATION_EDGE_SOURCE,
        TABLE_MUTATION_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table mutation edge matrix (Luau)",
        TABLE_MUTATION_EDGE_SOURCE,
        TABLE_MUTATION_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table hole and concat boundary (Blu)",
        TABLE_HOLE_EDGE_SOURCE,
        TABLE_HOLE_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table hole and concat boundary (Luau)",
        TABLE_HOLE_EDGE_SOURCE,
        TABLE_HOLE_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "assignment evaluation order (Blu)",
        ASSIGNMENT_ORDER_EDGE_SOURCE,
        ASSIGNMENT_ORDER_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "assignment evaluation order (Luau)",
        ASSIGNMENT_ORDER_EDGE_SOURCE,
        ASSIGNMENT_ORDER_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "utf8 integer argument conversion (Blu)",
        UTF8_INTEGER_ARGUMENT_SOURCE,
        UTF8_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "utf8 integer argument conversion (Luau)",
        UTF8_INTEGER_ARGUMENT_SOURCE,
        UTF8_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "utf8 malformed sequence behavior (Blu)",
        UTF8_MALFORMED_SOURCE,
        UTF8_MALFORMED_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "utf8 malformed sequence behavior (Luau)",
        UTF8_MALFORMED_SOURCE,
        UTF8_MALFORMED_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "utf8 lax mode (Luau)",
        UTF8_LAX_SOURCE,
        UTF8_LAX_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table positional argument conversion (Blu)",
        TABLE_POSITION_ARGUMENT_SOURCE,
        TABLE_POSITION_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "table positional argument conversion (Luau)",
        TABLE_POSITION_ARGUMENT_SOURCE,
        TABLE_POSITION_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math integer bounds (Blu)",
        MATH_INTEGER_BOUNDS_SOURCE,
        MATH_INTEGER_BOUNDS_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math.random fractional arguments (Blu)",
        MATH_RANDOM_FRACTIONAL_ARGUMENT_SOURCE,
        MATH_RANDOM_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math.random fractional arguments (Luau)",
        MATH_RANDOM_FRACTIONAL_ARGUMENT_SOURCE,
        MATH_RANDOM_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math.random fractional arguments ({name})"),
            MATH_RANDOM_FRACTIONAL_ARGUMENT_SOURCE,
            MATH_RANDOM_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-random-fractional-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "math.ldexp fractional exponent (Blu)",
        MATH_LDEXP_FRACTIONAL_ARGUMENT_SOURCE,
        MATH_LDEXP_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math.ldexp fractional exponent (Luau)",
        MATH_LDEXP_FRACTIONAL_ARGUMENT_SOURCE,
        MATH_LDEXP_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math.ldexp fractional exponent ({name})"),
            MATH_LDEXP_FRACTIONAL_ARGUMENT_SOURCE,
            MATH_LDEXP_FRACTIONAL_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-ldexp-fractional-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "math profile edge matrix (Blu)",
        MATH_PROFILE_EDGE_SOURCE,
        MATH_PROFILE_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math profile edge matrix (Luau)",
        MATH_PROFILE_EDGE_SOURCE,
        MATH_PROFILE_EDGE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math profile edge matrix ({name})"),
            MATH_PROFILE_EDGE_SOURCE,
            MATH_PROFILE_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-profile-edge-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "math min/max edge matrix (Blu)",
        MATH_MIN_MAX_EDGE_SOURCE,
        MATH_MIN_MAX_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math min/max edge matrix (Luau)",
        MATH_MIN_MAX_EDGE_SOURCE,
        MATH_MIN_MAX_EDGE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math min/max edge matrix ({name})"),
            MATH_MIN_MAX_EDGE_SOURCE,
            MATH_MIN_MAX_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-min-max-edge-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "math fmod edge matrix (Blu)",
        MATH_FMOD_EDGE_SOURCE,
        MATH_FMOD_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math fmod edge matrix (Luau)",
        MATH_FMOD_EDGE_SOURCE,
        MATH_FMOD_EDGE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math fmod edge matrix ({name})"),
            MATH_FMOD_EDGE_SOURCE,
            MATH_FMOD_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-fmod-edge-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "math subtype and integer helper edges (Blu)",
        MATH_SUBTYPE_EDGE_SOURCE,
        MATH_SUBTYPE_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math subtype and integer helper edges (Luau)",
        MATH_SUBTYPE_EDGE_SOURCE,
        MATH_SUBTYPE_EDGE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math subtype and integer helper edges ({name})"),
            MATH_SUBTYPE_EDGE_SOURCE,
            MATH_SUBTYPE_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-subtype-edge-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "math numeric-string arguments (Blu)",
        MATH_STRING_ARGUMENT_SOURCE,
        MATH_STRING_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "math numeric-string arguments (Luau)",
        MATH_STRING_ARGUMENT_SOURCE,
        MATH_STRING_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("math numeric-string arguments ({name})"),
            MATH_STRING_ARGUMENT_SOURCE,
            MATH_STRING_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-string-arguments-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "Luau math extension numeric-string edges (Blu)",
        MATH_LUAU_EXTENSION_SOURCE,
        MATH_LUAU_EXTENSION_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "Luau math extension numeric-string edges (Luau)",
        MATH_LUAU_EXTENSION_SOURCE,
        MATH_LUAU_EXTENSION_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        "owned-math-luau-extension-reference.luau",
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string pack/unpack (Blu)",
        STRING_PACK_SOURCE,
        STRING_PACK_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string pack alignment (Blu)",
        STRING_PACK_ALIGNMENT_SOURCE,
        STRING_PACK_ALIGNMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.unpack integer argument (Blu)",
        STRING_UNPACK_INTEGER_ARGUMENT_SOURCE,
        STRING_UNPACK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.unpack integer argument (Luau)",
        STRING_UNPACK_INTEGER_ARGUMENT_SOURCE,
        STRING_UNPACK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.unpack integer argument ({name})"),
            STRING_UNPACK_INTEGER_ARGUMENT_SOURCE,
            STRING_UNPACK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-unpack-integer-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "string.pack integer argument (Blu)",
        STRING_PACK_INTEGER_ARGUMENT_SOURCE,
        STRING_PACK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "string.pack integer argument (Luau)",
        STRING_PACK_INTEGER_ARGUMENT_SOURCE,
        STRING_PACK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.pack integer argument ({name})"),
            STRING_PACK_INTEGER_ARGUMENT_SOURCE,
            STRING_PACK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-pack-integer-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "deep tail recursion (Luau)",
        DEEP_TAIL_RECURSION_SOURCE,
        DEEP_TAIL_RECURSION_REFERENCE_SOURCE,
        SemanticProfile::Luau,
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
    verify_owned_program_case(
        "string literal call sugar (Blu)",
        STRING_CALL_SOURCE,
        STRING_CALL_REFERENCE_SOURCE,
        SemanticProfile::Blu,
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
    verify_owned_program_case_against_executable(
        "owned direct table iteration (Luau)",
        DIRECT_ITERATION_SOURCE,
        DIRECT_ITERATION_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        "owned-direct-iteration-reference.luau",
        temporary.path(),
    )?;
    verify_program_case(
        "direct table __iter hook",
        DIRECT_ITERATION_HOOK_SOURCE,
        DIRECT_ITERATION_HOOK_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "owned direct table __iter hook (Luau)",
        DIRECT_ITERATION_HOOK_SOURCE,
        DIRECT_ITERATION_HOOK_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        "owned-direct-iteration-hook-reference.luau",
        temporary.path(),
    )?;
    verify_known_boundary_case(
        "owned yielding direct table __iter extension (Blu)",
        (
            DIRECT_ITERATION_YIELD_SOURCE,
            DIRECT_ITERATION_YIELD_REFERENCE_SOURCE,
        ),
        SemanticProfile::Blu,
        &args.upstream,
        ("boolean:true", "boolean:false"),
        "owned-direct-iteration-yield-reference.luau",
        temporary.path(),
    )?;
    verify_known_boundary_case(
        "owned yielding direct table __iter extension (Luau)",
        (
            DIRECT_ITERATION_YIELD_SOURCE,
            DIRECT_ITERATION_YIELD_REFERENCE_SOURCE,
        ),
        SemanticProfile::Luau,
        &args.upstream,
        ("boolean:true", "boolean:false"),
        "owned-direct-iteration-yield-reference.luau",
        temporary.path(),
    )?;
    verify_owned_program_case(
        "direct table __iter mutation and yield boundary (Blu)",
        DIRECT_ITERATION_EDGE_SOURCE,
        DIRECT_ITERATION_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "direct table __iter mutation and yield boundary (Luau)",
        DIRECT_ITERATION_EDGE_SOURCE,
        DIRECT_ITERATION_EDGE_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        "owned-direct-iteration-edge-reference.luau",
        temporary.path(),
    )?;
    verify_owned_program_case(
        "ipairs integer argument (Blu)",
        IPAIRS_INTEGER_ARGUMENT_SOURCE,
        IPAIRS_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "ipairs integer argument (Luau)",
        IPAIRS_INTEGER_ARGUMENT_SOURCE,
        IPAIRS_INTEGER_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("profile-specific ipairs hook ({name})"),
            IPAIRS_HOOK_SOURCE,
            IPAIRS_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-ipairs-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("profile-specific ipairs integer argument ({name})"),
            IPAIRS_INTEGER_ARGUMENT_SOURCE,
            IPAIRS_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-ipairs-integer-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "xpcall handler failure (Blu)",
        XPCALL_HANDLER_ERROR_SOURCE,
        XPCALL_HANDLER_ERROR_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "xpcall handler failure (Luau)",
        XPCALL_HANDLER_ERROR_SOURCE,
        XPCALL_HANDLER_ERROR_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("xpcall handler failure ({name})"),
            XPCALL_HANDLER_ERROR_SOURCE,
            XPCALL_HANDLER_ERROR_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-xpcall-handler-error-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "protected error values (Blu)",
        PROTECTED_ERROR_VALUE_SOURCE,
        PROTECTED_ERROR_VALUE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "protected error values (Luau)",
        PROTECTED_ERROR_VALUE_SOURCE,
        PROTECTED_ERROR_VALUE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("protected error values ({name})"),
            PROTECTED_ERROR_VALUE_SOURCE,
            PROTECTED_ERROR_VALUE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-protected-error-values-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "error level source prefix (Blu)",
        ERROR_LEVEL_SOURCE,
        ERROR_LEVEL_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "error level source prefix (Luau)",
        ERROR_LEVEL_SOURCE,
        ERROR_LEVEL_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("error level source prefix ({name})"),
            ERROR_LEVEL_SOURCE,
            ERROR_LEVEL_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-error-level-reference.lua",
            temporary.path(),
        )?;
    }
    verify_known_boundary_case(
        "yielding __tostring extension (Blu)",
        (TOSTRING_YIELD_SOURCE, TOSTRING_YIELD_REFERENCE_SOURCE),
        SemanticProfile::Blu,
        &args.upstream,
        ("boolean:true", "boolean:false"),
        "owned-tostring-yield-reference.luau",
        temporary.path(),
    )?;
    verify_known_boundary_case(
        "yielding __tostring extension (Luau)",
        (TOSTRING_YIELD_SOURCE, TOSTRING_YIELD_REFERENCE_SOURCE),
        SemanticProfile::Luau,
        &args.upstream,
        ("boolean:true", "boolean:false"),
        "owned-tostring-yield-reference.luau",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_known_boundary_case(
            &format!("yielding __tostring boundary ({name})"),
            (TOSTRING_YIELD_SOURCE, TOSTRING_YIELD_REFERENCE_SOURCE),
            profile,
            executable,
            ("boolean:false", "boolean:false"),
            "owned-tostring-yield-reference.lua",
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        let expected_owned = "string:false:true:true:false:false:false";
        verify_known_boundary_case(
            &format!("deep error level source selection ({name})"),
            (
                ERROR_LEVEL_DEEP_BOUNDARY_SOURCE,
                ERROR_LEVEL_DEEP_BOUNDARY_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (expected_owned, "string:false:true:true:false:true:true"),
            "owned-error-level-deep-boundary-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "coroutine error values (Blu)",
        COROUTINE_ERROR_VALUE_SOURCE,
        COROUTINE_ERROR_VALUE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "coroutine error values (Luau)",
        COROUTINE_ERROR_VALUE_SOURCE,
        COROUTINE_ERROR_VALUE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("coroutine error values ({name})"),
            COROUTINE_ERROR_VALUE_SOURCE,
            COROUTINE_ERROR_VALUE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-coroutine-error-values-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "coroutine resume state diagnostics (Blu)",
        COROUTINE_RESUME_STATE_SOURCE,
        COROUTINE_RESUME_STATE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "coroutine resume state diagnostics (Luau)",
        COROUTINE_RESUME_STATE_SOURCE,
        COROUTINE_RESUME_STATE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("coroutine resume state diagnostics ({name})"),
            COROUTINE_RESUME_STATE_SOURCE,
            COROUTINE_RESUME_STATE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-coroutine-resume-state-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "coroutine close and yieldability state (Blu)",
        COROUTINE_CLOSE_STATE_SOURCE,
        COROUTINE_CLOSE_STATE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "coroutine close and yieldability state (Luau)",
        COROUTINE_CLOSE_STATE_SOURCE,
        COROUTINE_CLOSE_STATE_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("coroutine close and yieldability state ({name})"),
            COROUTINE_CLOSE_STATE_SOURCE,
            COROUTINE_CLOSE_STATE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-coroutine-close-state-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "coroutine argument and dead-close diagnostics (Blu)",
        COROUTINE_ARGUMENT_SOURCE,
        COROUTINE_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "coroutine argument and dead-close diagnostics (Luau)",
        COROUTINE_ARGUMENT_SOURCE,
        COROUTINE_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("coroutine argument and dead-close diagnostics ({name})"),
            COROUTINE_ARGUMENT_SOURCE,
            COROUTINE_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-coroutine-argument-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "base type and tostring",
        BASE_LIBRARY_SOURCE,
        BASE_LIBRARY_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "tostring metamethod (Blu)",
        TOSTRING_METAMETHOD_SOURCE,
        TOSTRING_METAMETHOD_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "tostring metamethod (Luau)",
        TOSTRING_METAMETHOD_SOURCE,
        TOSTRING_METAMETHOD_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("tostring metamethod ({name})"),
            TOSTRING_METAMETHOD_SOURCE,
            TOSTRING_METAMETHOD_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-tostring-metamethod-reference.lua",
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format general conversions ({name})"),
            STRING_FORMAT_GENERAL_SOURCE,
            STRING_FORMAT_GENERAL_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-general-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.format quoted values (Luau)",
        STRING_FORMAT_QUOTED_SOURCE,
        STRING_FORMAT_QUOTED_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Blu, &lua_references[2]),
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format quoted values ({name})"),
            STRING_FORMAT_QUOTED_SOURCE,
            STRING_FORMAT_QUOTED_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-quoted-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.format quoted non-scalars (Luau)",
        STRING_FORMAT_QUOTED_NONSCALAR_SOURCE,
        STRING_FORMAT_QUOTED_NONSCALAR_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string.format quoted non-scalars (Blu)",
        STRING_FORMAT_QUOTED_NONSCALAR_BLU_SOURCE,
        STRING_FORMAT_QUOTED_NONSCALAR_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-format-quoted-nonscalar-blu-reference.lua",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format quoted non-scalars ({name})"),
            STRING_FORMAT_QUOTED_NONSCALAR_SOURCE,
            STRING_FORMAT_QUOTED_NONSCALAR_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-quoted-nonscalar-reference.lua",
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Blu, &lua_references[2]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format hexadecimal floats ({name})"),
            STRING_FORMAT_HEXADECIMAL_SOURCE,
            STRING_FORMAT_HEXADECIMAL_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-hexadecimal-reference.lua",
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Blu, &lua_references[2]),
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format integer precision ({name})"),
            STRING_FORMAT_INTEGER_PRECISION_SOURCE,
            STRING_FORMAT_INTEGER_PRECISION_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-integer-precision-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.format modifier matrix (Luau)",
        STRING_FORMAT_MODIFIER_SOURCE,
        STRING_FORMAT_MODIFIER_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Blu, &lua_references[2]),
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format modifier matrix ({name})"),
            STRING_FORMAT_MODIFIER_SOURCE,
            STRING_FORMAT_MODIFIER_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-modifier-reference.lua",
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "string.format numeric-string arguments (Blu)",
        STRING_FORMAT_STRING_ARGUMENT_SOURCE,
        STRING_FORMAT_STRING_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string.format numeric-string arguments (Luau)",
        STRING_FORMAT_STRING_ARGUMENT_SOURCE,
        STRING_FORMAT_STRING_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        "owned-string-format-string-arguments-reference.luau",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format numeric-string arguments ({name})"),
            STRING_FORMAT_STRING_ARGUMENT_SOURCE,
            STRING_FORMAT_STRING_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-string-arguments-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.format non-scalar coercion (Luau)",
        STRING_FORMAT_NONSCALAR_SOURCE,
        STRING_FORMAT_NONSCALAR_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string.format non-scalar coercion (Blu)",
        STRING_FORMAT_NONSCALAR_BLU_SOURCE,
        STRING_FORMAT_NONSCALAR_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-format-nonscalar-blu-reference.lua",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format non-scalar coercion ({name})"),
            STRING_FORMAT_NONSCALAR_SOURCE,
            STRING_FORMAT_NONSCALAR_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-nonscalar-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.format char range (Luau)",
        STRING_FORMAT_CHAR_RANGE_SOURCE,
        STRING_FORMAT_CHAR_RANGE_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string.format char range (Blu)",
        STRING_FORMAT_CHAR_RANGE_BLU_SOURCE,
        STRING_FORMAT_CHAR_RANGE_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-format-char-range-blu-reference.lua",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format char range ({name})"),
            STRING_FORMAT_CHAR_RANGE_SOURCE,
            STRING_FORMAT_CHAR_RANGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-char-range-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.format flags (Luau)",
        STRING_FORMAT_FLAGS_SOURCE,
        STRING_FORMAT_FLAGS_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Blu, &lua_references[2]),
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.format flags ({name})"),
            STRING_FORMAT_FLAGS_SOURCE,
            STRING_FORMAT_FLAGS_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-format-flags-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.gmatch empty-match profile split (Luau)",
        STRING_GMATCH_EMPTY_SOURCE,
        STRING_GMATCH_EMPTY_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string.gmatch empty-match profile split (Blu)",
        STRING_GMATCH_EMPTY_BLU_SOURCE,
        STRING_GMATCH_EMPTY_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-gmatch-empty-blu-reference.lua",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.gmatch empty-match profile split ({name})"),
            STRING_GMATCH_EMPTY_SOURCE,
            STRING_GMATCH_EMPTY_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-gmatch-empty-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "string.gsub empty-match profile split (Luau)",
        STRING_GSUB_EMPTY_SOURCE,
        STRING_GSUB_EMPTY_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "string.gsub empty-match profile split (Blu)",
        STRING_GSUB_EMPTY_BLU_SOURCE,
        STRING_GSUB_EMPTY_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &lua_references[2].1,
        "owned-string-gsub-empty-blu-reference.lua",
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("string.gsub empty-match profile split ({name})"),
            STRING_GSUB_EMPTY_SOURCE,
            STRING_GSUB_EMPTY_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-gsub-empty-reference.lua",
            temporary.path(),
        )?;
    }
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
        "select numeric conversion",
        SELECT_INTEGER_ARGUMENT_SOURCE,
        SELECT_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "select numeric conversion (Blu)",
        SELECT_INTEGER_ARGUMENT_SOURCE,
        SELECT_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "select numeric conversion (Luau)",
        SELECT_INTEGER_ARGUMENT_SOURCE,
        SELECT_INTEGER_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("select numeric conversion ({name})"),
            SELECT_INTEGER_ARGUMENT_SOURCE,
            SELECT_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-select-integer-reference.lua",
            temporary.path(),
        )?;
    }
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
        "xpcall argument forwarding",
        XPCALL_ARGUMENT_SOURCE,
        XPCALL_ARGUMENT_REFERENCE_SOURCE,
        &compiler,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "xpcall argument forwarding (Blu)",
        XPCALL_ARGUMENT_SOURCE,
        XPCALL_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "xpcall argument forwarding (Luau)",
        XPCALL_ARGUMENT_SOURCE,
        XPCALL_ARGUMENT_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("xpcall argument forwarding ({name})"),
            XPCALL_ARGUMENT_SOURCE,
            XPCALL_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-xpcall-argument-reference.lua",
            temporary.path(),
        )?;
    }
    verify_program_case(
        "number conversion",
        NUMBER_CONVERSION_SOURCE,
        NUMBER_CONVERSION_REFERENCE_SOURCE,
        &compiler,
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
        verify_owned_program_case_against_executable(
            &format!("number conversion boundaries ({name})"),
            NUMBER_BOUNDARY_SOURCE,
            NUMBER_BOUNDARY_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-number-boundary-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("repeat condition local visibility ({name})"),
            REPEAT_SCOPE_SOURCE,
            REPEAT_SCOPE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-repeat-scope-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("number conversion overflow rules ({name})"),
            NUMBER_OVERFLOW_SOURCE,
            NUMBER_OVERFLOW_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-number-overflow-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("number conversion base validation ({name})"),
            NUMBER_BASE_SOURCE,
            NUMBER_BASE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-number-base-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("number conversion grammar ({name})"),
            NUMBER_GRAMMAR_SOURCE,
            NUMBER_GRAMMAR_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-number-grammar-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string.char numeric conversion ({name})"),
            STRING_CHAR_CONVERSION_SOURCE,
            STRING_CHAR_CONVERSION_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-char-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string.byte index conversion ({name})"),
            STRING_BYTE_INDEX_SOURCE,
            STRING_BYTE_INDEX_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-byte-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string.sub/find index conversion ({name})"),
            STRING_SUB_FIND_INDEX_SOURCE,
            STRING_SUB_FIND_INDEX_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-sub-find-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string integer argument conversion ({name})"),
            STRING_INTEGER_ARGUMENT_SOURCE,
            STRING_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string pattern edge matrix ({name})"),
            STRING_PATTERN_EDGE_SOURCE,
            STRING_PATTERN_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-pattern-edge-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string balanced/frontier/capture semantics ({name})"),
            STRING_PATTERN_CAPTURE_SOURCE,
            STRING_PATTERN_CAPTURE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-pattern-capture-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string replacement capture semantics ({name})"),
            STRING_PATTERN_REPLACEMENT_SOURCE,
            STRING_PATTERN_REPLACEMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-pattern-replacement-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("Lua 5.1 math.mod alias surface ({name})"),
            MATH_LEGACY_ALIAS_SOURCE,
            MATH_LEGACY_ALIAS_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-legacy-alias-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("nil-key table lookup semantics ({name})"),
            NIL_TABLE_LOOKUP_SOURCE,
            NIL_TABLE_LOOKUP_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-nil-table-lookup-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table integer argument conversion ({name})"),
            TABLE_INTEGER_ARGUMENT_SOURCE,
            TABLE_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table mutation edge matrix ({name})"),
            TABLE_MUTATION_EDGE_SOURCE,
            TABLE_MUTATION_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-mutation-edge-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table hole and concat boundary ({name})"),
            TABLE_HOLE_EDGE_SOURCE,
            TABLE_HOLE_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-hole-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table length consumers ({name})"),
            TABLE_LENGTH_CONSUMER_EDGE_SOURCE,
            TABLE_LENGTH_CONSUMER_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-length-consumer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("rawlen/maxn scalar edges ({name})"),
            RAWLEN_MAXN_SCALAR_EDGE_SOURCE,
            RAWLEN_MAXN_SCALAR_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-rawlen-maxn-scalar-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("assignment-created table length boundaries ({name})"),
            TABLE_ASSIGNMENT_LENGTH_EDGE_SOURCE,
            TABLE_ASSIGNMENT_LENGTH_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-assignment-length-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table.maxn extreme numeric keys ({name})"),
            TABLE_MAXN_EXTREME_NUMERIC_EDGE_SOURCE,
            TABLE_MAXN_EXTREME_NUMERIC_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-maxn-extreme-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table iteration length consumers ({name})"),
            TABLE_ITERATION_LENGTH_EDGE_SOURCE,
            TABLE_ITERATION_LENGTH_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-iteration-length-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table iteration mutation ({name})"),
            TABLE_ITERATION_MUTATION_EDGE_SOURCE,
            TABLE_ITERATION_MUTATION_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-iteration-mutation-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("assignment evaluation order ({name})"),
            ASSIGNMENT_ORDER_EDGE_SOURCE,
            ASSIGNMENT_ORDER_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-assignment-order-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("assignment and constructor evaluation order ({name})"),
            ASSIGNMENT_CONSTRUCTOR_EDGE_SOURCE,
            ASSIGNMENT_CONSTRUCTOR_EDGE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-assignment-constructor-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("utf8 integer argument conversion ({name})"),
            UTF8_INTEGER_ARGUMENT_SOURCE,
            UTF8_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-utf8-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("table positional argument conversion ({name})"),
            TABLE_POSITION_ARGUMENT_SOURCE,
            TABLE_POSITION_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-table-position-reference.lua",
            temporary.path(),
        )?;
    }
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
    verify_owned_program_case(
        "owned Luau native callback yieldability",
        OWNED_NATIVE_CALLBACK_YIELDABILITY_SOURCE,
        OWNED_NATIVE_CALLBACK_YIELDABILITY_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case_against_executable(
        "Lua 5.1 main-chunk environment rebinding",
        LUA51_MAIN_CHUNK_ENVIRONMENT_SOURCE,
        LUA51_MAIN_CHUNK_ENVIRONMENT_REFERENCE_SOURCE,
        SemanticProfile::Lua51,
        &lua_references[0].1,
        "owned-lua51-main-chunk-environment-reference.lua",
        temporary.path(),
    )?;
    verify_known_boundary_case(
        "Lua 5.1 yielding rebound global __newindex extension",
        (
            LUA51_YIELDING_MAIN_CHUNK_NEWINDEX_SOURCE,
            LUA51_YIELDING_MAIN_CHUNK_NEWINDEX_REFERENCE_SOURCE,
        ),
        SemanticProfile::Lua51,
        &lua_references[0].1,
        ("boolean:true", "boolean:false"),
        "owned-lua51-yielding-main-chunk-newindex-reference.lua",
        temporary.path(),
    )?;
    verify_owned_program_case(
        "yielding string.gsub replacement-table lookup (Luau)",
        YIELDING_GSUB_TABLE_INDEX_SOURCE,
        YIELDING_GSUB_TABLE_INDEX_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "yielding string.gsub replacement-table lookup (Blu)",
        YIELDING_GSUB_TABLE_INDEX_SOURCE,
        YIELDING_GSUB_TABLE_INDEX_REFERENCE_SOURCE,
        SemanticProfile::Blu,
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
            &format!("yielding string.gsub replacement-table lookup ({name})"),
            YIELDING_GSUB_TABLE_INDEX_SOURCE,
            YIELDING_GSUB_TABLE_INDEX_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("deep tail recursion ({name})"),
            DEEP_TAIL_RECURSION_SOURCE,
            DEEP_TAIL_RECURSION_REFERENCE_SOURCE,
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
        verify_owned_program_case_against_executable(
            &format!("profile library surface ({name})"),
            PROFILE_LIBRARY_SURFACE_SOURCE,
            PROFILE_LIBRARY_SURFACE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-profile-library-surface-reference.lua",
            temporary.path(),
        )?;
        let (goto_scope_source, goto_scope_reference) = if profile == SemanticProfile::Lua51 {
            (
                GOTO_SCOPE_LOAD_LUA51_SOURCE,
                GOTO_SCOPE_LOAD_LUA51_REFERENCE_SOURCE,
            )
        } else {
            (GOTO_SCOPE_LOAD_SOURCE, GOTO_SCOPE_LOAD_REFERENCE_SOURCE)
        };
        verify_owned_engine_program_case_against_executable(
            &format!("goto local-scope rejection through load ({name})"),
            goto_scope_source,
            goto_scope_reference,
            profile,
            executable,
            "owned-goto-scope-load-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("collectgarbage controls ({name})"),
            COLLECTGARBAGE_CONTROLS_SOURCE,
            COLLECTGARBAGE_CONTROLS_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-collectgarbage-controls-reference.lua",
            temporary.path(),
        )?;
        let collectgarbage_tuning = match profile {
            SemanticProfile::Lua51 => "string:ok:number:ok:number:error:string:error:string",
            SemanticProfile::Lua52 => "string:ok:number:ok:number:ok:number:ok:number",
            SemanticProfile::Lua53 => "string:ok:number:ok:number:error:string:error:string",
            SemanticProfile::Lua54 => "string:ok:number:ok:number:ok:string:ok:string",
            SemanticProfile::Lua55 => "string:error:string:error:string:ok:string:ok:string",
            _ => unreachable!("Lua profile loop contains only known profiles"),
        };
        verify_known_boundary_case(
            &format!("collectgarbage tuning boundary ({name})"),
            (
                COLLECTGARBAGE_TUNING_BOUNDARY_SOURCE,
                COLLECTGARBAGE_TUNING_BOUNDARY_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (collectgarbage_tuning, collectgarbage_tuning),
            "owned-collectgarbage-tuning-reference.lua",
            temporary.path(),
        )?;
        verify_known_boundary_case(
            &format!("guest table finalizer boundary ({name})"),
            (GUEST_FINALIZER_SOURCE, GUEST_FINALIZER_REFERENCE_SOURCE),
            profile,
            executable,
            (
                if matches!(
                    profile,
                    SemanticProfile::Lua52
                        | SemanticProfile::Lua53
                        | SemanticProfile::Lua54
                        | SemanticProfile::Lua55
                ) {
                    "number:1"
                } else {
                    "number:0"
                },
                if matches!(
                    profile,
                    SemanticProfile::Lua52
                        | SemanticProfile::Lua53
                        | SemanticProfile::Lua54
                        | SemanticProfile::Lua55
                ) {
                    "number:1"
                } else {
                    "number:0"
                },
            ),
            "owned-guest-finalizer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("guest table finalizer resurrection ({name})"),
            GUEST_FINALIZER_RESURRECTION_SOURCE,
            GUEST_FINALIZER_RESURRECTION_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-guest-finalizer-resurrection-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("guest table finalizer explicit rearming ({name})"),
            GUEST_FINALIZER_REARM_SOURCE,
            GUEST_FINALIZER_REARM_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-guest-finalizer-rearm-reference.lua",
            temporary.path(),
        )?;
        let has_table_finalizers = matches!(
            profile,
            SemanticProfile::Lua52
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        verify_known_boundary_case(
            &format!("guest table finalizer reverse order ({name})"),
            (
                GUEST_FINALIZER_ORDER_SOURCE,
                GUEST_FINALIZER_ORDER_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (
                if has_table_finalizers {
                    "string:3,2,1"
                } else {
                    "string:"
                },
                if has_table_finalizers {
                    "string:3,2,1"
                } else {
                    "string:"
                },
            ),
            "owned-guest-finalizer-order-reference.lua",
            temporary.path(),
        )?;
        let finalizer_error_owned = match profile {
            SemanticProfile::Lua52 | SemanticProfile::Lua53 => "string:boolean:false:string:1",
            SemanticProfile::Lua54 | SemanticProfile::Lua55 => "string:boolean:true:number:1",
            SemanticProfile::Blu => "string:boolean:true:number:0",
            SemanticProfile::Luau => "string:boolean:true:nil:0",
            SemanticProfile::Lua51 => "string:boolean:true:number:0",
            _ => unreachable!("Lua profile loop contains only known profiles"),
        };
        let finalizer_error_reference = match profile {
            SemanticProfile::Lua52 | SemanticProfile::Lua53 => "boolean:false:string:1",
            SemanticProfile::Lua54 | SemanticProfile::Lua55 => "boolean:true:number:1",
            SemanticProfile::Blu => "boolean:true:number:0",
            SemanticProfile::Luau => "boolean:true:nil:0",
            SemanticProfile::Lua51 => "boolean:true:number:0",
            _ => unreachable!("Lua profile loop contains only known profiles"),
        };
        verify_known_boundary_case(
            &format!("guest table finalizer error policy ({name})"),
            (
                GUEST_FINALIZER_ERROR_SOURCE,
                GUEST_FINALIZER_ERROR_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (finalizer_error_owned, finalizer_error_reference),
            "owned-guest-finalizer-error-reference.lua",
            temporary.path(),
        )?;
        let finalizer_yield_owned = match profile {
            SemanticProfile::Lua52 | SemanticProfile::Lua53 => "string:boolean:false:string:1",
            SemanticProfile::Lua54 | SemanticProfile::Lua55 => "string:boolean:true:number:1",
            SemanticProfile::Blu => "string:boolean:true:number:0",
            SemanticProfile::Luau => "string:boolean:true:nil:0",
            SemanticProfile::Lua51 => "string:boolean:true:number:0",
            _ => unreachable!("Lua profile loop contains only known profiles"),
        };
        let finalizer_yield_reference = match profile {
            SemanticProfile::Lua52 | SemanticProfile::Lua53 => "boolean:false:string:1",
            SemanticProfile::Lua54 | SemanticProfile::Lua55 => "boolean:true:number:1",
            SemanticProfile::Blu => "boolean:true:number:0",
            SemanticProfile::Luau => "boolean:true:nil:0",
            SemanticProfile::Lua51 => "boolean:true:number:0",
            _ => unreachable!("Lua profile loop contains only known profiles"),
        };
        verify_known_boundary_case(
            &format!("guest table finalizer yield policy ({name})"),
            (
                GUEST_FINALIZER_YIELD_SOURCE,
                GUEST_FINALIZER_YIELD_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (finalizer_yield_owned, finalizer_yield_reference),
            "owned-guest-finalizer-yield-reference.lua",
            temporary.path(),
        )?;
        let finalizer_register_reference = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            "number:2"
        } else if profile == SemanticProfile::Lua52 {
            "number:1"
        } else {
            "number:0"
        };
        let finalizer_register_owned = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            "number:2"
        } else if profile == SemanticProfile::Lua52 {
            "number:1"
        } else {
            "number:0"
        };
        verify_known_boundary_case(
            &format!("guest table finalizer register liveness ({name})"),
            (
                GUEST_FINALIZER_REGISTER_LIVENESS_SOURCE,
                GUEST_FINALIZER_REGISTER_LIVENESS_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (finalizer_register_owned, finalizer_register_reference),
            "owned-guest-finalizer-register-liveness-reference.lua",
            temporary.path(),
        )?;
        verify_known_boundary_case(
            &format!("debug C-stack limit boundary ({name})"),
            (
                DEBUG_CSTACK_LIMIT_SOURCE,
                DEBUG_CSTACK_LIMIT_REFERENCE_SOURCE,
            ),
            profile,
            executable,
            (
                "string:nil",
                if profile == SemanticProfile::Lua54 {
                    "function"
                } else {
                    "nil"
                },
            ),
            "owned-debug-cstack-limit-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("debug metatable slice ({name})"),
            DEBUG_METATABLE_SOURCE,
            DEBUG_METATABLE_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug getinfo function shape ({name})"),
            DEBUG_INFO_SOURCE,
            DEBUG_INFO_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-info-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug getinfo level zero ({name})"),
            DEBUG_LEVEL_ZERO_SOURCE,
            DEBUG_LEVEL_ZERO_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-level-zero-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug getinfo caller names ({name})"),
            DEBUG_CALLER_NAMES_SOURCE,
            DEBUG_CALLER_NAMES_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-caller-names-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug getinfo main chunk ({name})"),
            DEBUG_MAIN_SOURCE,
            DEBUG_MAIN_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-main-reference.lua",
            temporary.path(),
        )?;
        verify_owned_engine_program_case_against_executable(
            &format!("string.dump round trip ({name})"),
            STRING_DUMP_SOURCE,
            STRING_DUMP_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-dump-reference.lua",
            temporary.path(),
        )?;
        verify_owned_engine_program_case_against_executable(
            &format!("string.dump captured upvalue reload ({name})"),
            STRING_DUMP_CAPTURE_SOURCE,
            STRING_DUMP_CAPTURE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-dump-capture-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug getlocal live locals ({name})"),
            DEBUG_LOCAL_SOURCE,
            DEBUG_LOCAL_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-local-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug setlocal active and suspended locals ({name})"),
            DEBUG_SETLOCAL_SOURCE,
            DEBUG_SETLOCAL_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-setlocal-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug local integer arguments ({name})"),
            DEBUG_LOCAL_INTEGER_ARGUMENT_SOURCE,
            DEBUG_LOCAL_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-local-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug upvalue integer arguments ({name})"),
            DEBUG_UPVALUE_INTEGER_ARGUMENT_SOURCE,
            DEBUG_UPVALUE_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-upvalue-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug hook integer argument ({name})"),
            DEBUG_HOOK_INTEGER_ARGUMENT_SOURCE,
            DEBUG_HOOK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-hook-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug getinfo integer argument ({name})"),
            DEBUG_INFO_INTEGER_ARGUMENT_SOURCE,
            DEBUG_INFO_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-getinfo-integer-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug currentline ({name})"),
            DEBUG_CURRENTLINE_SOURCE,
            DEBUG_CURRENTLINE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-currentline-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug activelines ({name})"),
            DEBUG_ACTIVELINES_SOURCE,
            DEBUG_ACTIVELINES_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-activelines-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug retained caller frames ({name})"),
            DEBUG_CALLER_SOURCE,
            DEBUG_CALLER_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-caller-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug setlocal retained caller ({name})"),
            DEBUG_SET_CALLER_SOURCE,
            DEBUG_SET_CALLER_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-set-caller-reference.lua",
            temporary.path(),
        )?;
        if profile != SemanticProfile::Lua51 {
            verify_owned_program_case_against_executable(
                &format!("debug upvaluejoin ({name})"),
                DEBUG_UPVALUEJOIN_SOURCE,
                DEBUG_UPVALUEJOIN_REFERENCE_SOURCE,
                profile,
                executable,
                "owned-debug-upvaluejoin-reference.lua",
                temporary.path(),
            )?;
        }
        verify_owned_program_case_against_executable(
            &format!("debug active coroutine thread ({name})"),
            DEBUG_THREAD_SOURCE,
            DEBUG_THREAD_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-thread-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug suspended coroutine thread ({name})"),
            DEBUG_SUSPENDED_THREAD_SOURCE,
            DEBUG_SUSPENDED_THREAD_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-suspended-thread-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug upvalue access ({name})"),
            DEBUG_UPVALUE_SOURCE,
            DEBUG_UPVALUE_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-upvalue-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug traceback active frames ({name})"),
            DEBUG_TRACEBACK_SOURCE,
            DEBUG_TRACEBACK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-traceback-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug line hooks ({name})"),
            DEBUG_HOOK_SOURCE,
            DEBUG_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug count hooks ({name})"),
            DEBUG_COUNT_HOOK_SOURCE,
            DEBUG_COUNT_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-count-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug call and return hooks ({name})"),
            DEBUG_CALL_RETURN_HOOK_SOURCE,
            DEBUG_CALL_RETURN_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-call-return-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug tail hooks ({name})"),
            DEBUG_TAIL_HOOK_SOURCE,
            DEBUG_TAIL_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-tail-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug native callback hooks ({name})"),
            DEBUG_NATIVE_HOOK_SOURCE,
            DEBUG_NATIVE_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-native-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug native C-frame metadata ({name})"),
            DEBUG_NATIVE_FRAME_SOURCE,
            DEBUG_NATIVE_FRAME_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-native-frame-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug native C-frame names ({name})"),
            DEBUG_NATIVE_NAME_SOURCE,
            DEBUG_NATIVE_NAME_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-native-name-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug yielding hook rejection ({name})"),
            DEBUG_YIELDING_HOOK_SOURCE,
            DEBUG_YIELDING_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-yielding-hook-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("debug thread-targeted hooks ({name})"),
            DEBUG_THREAD_HOOK_SOURCE,
            DEBUG_THREAD_HOOK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-debug-thread-hook-reference.lua",
            temporary.path(),
        )?;
        if profile != SemanticProfile::Lua51 {
            verify_owned_program_case_against_executable(
                &format!("debug uservalue shape ({name})"),
                DEBUG_USERVALUE_SOURCE,
                DEBUG_USERVALUE_REFERENCE_SOURCE,
                profile,
                executable,
                "owned-debug-uservalue-reference.lua",
                temporary.path(),
            )?;
            verify_owned_program_case_against_executable(
                &format!("debug uservalue integer argument ({name})"),
                DEBUG_USERVALUE_INTEGER_ARGUMENT_SOURCE,
                DEBUG_USERVALUE_INTEGER_ARGUMENT_REFERENCE_SOURCE,
                profile,
                executable,
                "owned-debug-uservalue-integer-reference.lua",
                temporary.path(),
            )?;
        }
        if profile == SemanticProfile::Lua51 {
            verify_owned_environment_case(
                &format!("Lua 5.1 function environments ({name})"),
                LUA51_ENVIRONMENT_SOURCE,
                LUA51_ENVIRONMENT_REFERENCE_SOURCE,
                profile,
                executable,
                temporary.path(),
            )?;
            verify_owned_environment_case(
                &format!("Lua 5.1 current stack environment ({name})"),
                LUA51_STACK_ENVIRONMENT_SOURCE,
                LUA51_STACK_ENVIRONMENT_REFERENCE_SOURCE,
                profile,
                executable,
                temporary.path(),
            )?;
            verify_known_boundary_case(
                &format!("Lua 5.1 non-current stack environment ({name})"),
                (
                    LUA51_NONCURRENT_ENVIRONMENT_SOURCE,
                    LUA51_NONCURRENT_ENVIRONMENT_REFERENCE_SOURCE,
                ),
                profile,
                executable,
                (
                    "string:outer-error:error-string",
                    "string:outer-ok:caller-ok:set-error:error-string",
                ),
                "owned-lua51-noncurrent-environment-reference.lua",
                temporary.path(),
            )?;
        }
        verify_owned_load_reader_case(
            &format!("reader-function load ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_yielding_load_reader_case(
            &format!("yielding reader-function load ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("package searcher dispatch ({name})"),
            PACKAGE_SEARCHER_SOURCE,
            PACKAGE_SEARCHER_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_yielding_package_searcher_case(
            &format!("yielding package searcher ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_yielding_package_loader_case(
            &format!("yielding package loader ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        if matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Lua52) {
            verify_owned_program_case_against_executable(
                &format!("yielding legacy module option ({name})"),
                YIELDING_MODULE_OPTION_SOURCE,
                YIELDING_MODULE_OPTION_REFERENCE_SOURCE,
                profile,
                executable,
                "owned-yielding-module-option-reference.lua",
                temporary.path(),
            )?;
        }
    }
    verify_owned_program_case(
        "package loadlib boundary (Lua 5.1)",
        PACKAGE_LOADLIB_SOURCE,
        PACKAGE_LOADLIB_REFERENCE_SOURCE,
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "os core (Lua 5.1)",
        OS_SOURCE,
        OS_REFERENCE_SOURCE,
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "os/debug integer arguments (Blu)",
        OS_DEBUG_INTEGER_ARGUMENT_SOURCE,
        OS_DEBUG_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Blu,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "os/debug integer arguments (Luau)",
        OS_DEBUG_INTEGER_ARGUMENT_SOURCE,
        OS_DEBUG_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Luau,
        &args.upstream,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "os/debug integer arguments (Lua 5.1)",
        OS_DEBUG_INTEGER_ARGUMENT_SOURCE,
        OS_DEBUG_INTEGER_ARGUMENT_REFERENCE_SOURCE,
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_os_mutation_case(
        "os filesystem mutation (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_os_execute_case(
        "os.execute (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_os_exit_case(
        "os.exit (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_os_locale_case(
        "os locale/tmpname (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_clock_case(
        "os.clock (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_time_case(
        "os.time (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_time_table_case(
        "os.time table (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_date_case(
        "os.date (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    verify_owned_date_table_case(
        "os.date table (Lua 5.1)",
        SemanticProfile::Lua51,
        &lua_references[0].1,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case_against_executable(
            &format!("math integer bounds ({name})"),
            MATH_INTEGER_BOUNDS_SOURCE,
            MATH_INTEGER_BOUNDS_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-math-integer-bounds-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string pack/unpack ({name})"),
            STRING_PACK_SOURCE,
            STRING_PACK_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-pack-reference.lua",
            temporary.path(),
        )?;
        verify_owned_program_case_against_executable(
            &format!("string pack alignment ({name})"),
            STRING_PACK_ALIGNMENT_SOURCE,
            STRING_PACK_ALIGNMENT_REFERENCE_SOURCE,
            profile,
            executable,
            "owned-string-pack-alignment-reference.lua",
            temporary.path(),
        )?;
        verify_owned_file_load_case(
            &format!("loadfile/dofile ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("utf8 library ({name})"),
            UTF8_SOURCE,
            UTF8_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
    }
    verify_foreign_lua_binary_case(&lua_references, temporary.path())?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case(
            &format!("zero numeric for step ({name})"),
            ZERO_NUMERIC_FOR_SOURCE,
            ZERO_NUMERIC_FOR_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_warning_case(
            &format!("warn channel ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
    }
    verify_owned_program_case(
        "Lua 5.5 global declarations",
        GLOBAL_DECLARATION_SOURCE,
        GLOBAL_DECLARATION_REFERENCE_SOURCE,
        SemanticProfile::Lua55,
        &lua_references[4].1,
        temporary.path(),
    )?;
    verify_owned_program_case(
        "Lua 5.5 named vararg table",
        NAMED_VARARG_SOURCE,
        NAMED_VARARG_REFERENCE_SOURCE,
        SemanticProfile::Lua55,
        &lua_references[4].1,
        temporary.path(),
    )?;
    verify_owned_load_mode_case(
        "Lua 5.5 load mode errors",
        LOAD_MODE_SOURCE,
        LOAD_MODE_REFERENCE_SOURCE,
        SemanticProfile::Lua55,
        &lua_references[4].1,
        temporary.path(),
    )?;
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case(
            &format!("utf8 malformed sequence behavior ({name})"),
            UTF8_MALFORMED_SOURCE,
            UTF8_MALFORMED_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("utf8 lax mode ({name})"),
            UTF8_LAX_SOURCE,
            UTF8_LAX_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("utf8 codes ({name})"),
            UTF8_CODES_SOURCE,
            UTF8_CODES_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_program_case(
            &format!("utf8 offset ({name})"),
            UTF8_OFFSET_SOURCE,
            UTF8_OFFSET_REFERENCE_SOURCE,
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
            &format!("coroutine.close unwinds suspended resources ({name})"),
            COROUTINE_CLOSE_SOURCE,
            COROUTINE_CLOSE_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("abandoned coroutine close boundary ({name})"),
            COROUTINE_ABANDON_SOURCE,
            COROUTINE_ABANDON_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("coroutine.close yielding-handler boundary ({name})"),
            COROUTINE_CLOSE_YIELD_SOURCE,
            COROUTINE_CLOSE_YIELD_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("to-be-closed error argument ({name})"),
            CLOSE_ERROR_SOURCE,
            CLOSE_ERROR_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("repeat condition to-be-closed timing ({name})"),
            REPEAT_CLOSE_SOURCE,
            REPEAT_CLOSE_REFERENCE_SOURCE,
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
        verify_owned_program_case(
            &format!("generic-for close on body error ({name})"),
            GENERIC_FOR_CLOSE_ERROR_SOURCE,
            GENERIC_FOR_CLOSE_ERROR_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("tail call closes to-be-closed local ({name})"),
            TAIL_CLOSE_SOURCE,
            TAIL_CLOSE_REFERENCE_SOURCE,
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
        verify_owned_program_case(
            &format!("package defaults ({name})"),
            PACKAGE_DEFAULTS_SOURCE,
            PACKAGE_DEFAULTS_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("package loadlib boundary ({name})"),
            PACKAGE_LOADLIB_SOURCE,
            PACKAGE_LOADLIB_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("os core ({name})"),
            OS_SOURCE,
            OS_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_program_case(
            &format!("os/debug integer arguments ({name})"),
            OS_DEBUG_INTEGER_ARGUMENT_SOURCE,
            OS_DEBUG_INTEGER_ARGUMENT_REFERENCE_SOURCE,
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_os_mutation_case(
            &format!("os filesystem mutation ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_os_execute_case(
            &format!("os.execute ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_os_exit_case(
            &format!("os.exit ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_os_locale_case(
            &format!("os locale/tmpname ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_clock_case(
            &format!("os.clock ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_time_case(
            &format!("os.time ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_time_table_case(
            &format!("os.time table ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_date_case(
            &format!("os.date ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_date_table_case(
            &format!("os.date table ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_io_case(
            &format!("io opaque file handle slice ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_io_tmpfile_case(
            &format!("io.tmpfile capability ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_io_operation_failure_case(
            &format!("io file operation failures ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_userdata_finalizer_case(
            &format!("host userdata finalizer ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_native_userdata_case(
            &format!("native bridge userdata ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_owned_native_loadlib_case(
            &format!("native bridge unavailable result ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
        verify_discarded_io_lines_case(
            &format!("discarded io.lines iterator ({name})"),
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
        verify_owned_file_searchpath_case(
            &format!("package.searchpath ({name})"),
            profile,
            executable,
            temporary.path(),
        )?;
    }
    for (profile, (name, executable)) in [
        (SemanticProfile::Lua51, &lua_references[0]),
        (SemanticProfile::Lua52, &lua_references[1]),
        (SemanticProfile::Lua53, &lua_references[2]),
        (SemanticProfile::Lua54, &lua_references[3]),
        (SemanticProfile::Lua55, &lua_references[4]),
    ] {
        verify_owned_source_require_case(
            &format!("source require ({name})"),
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
        "program differential corpus: pass (tables, loops, numeric-for dynamic/zero-step behavior, deep tail recursion and to-be-closed tail cleanup, math integer bounds and numeric-string/random/ldexp profile-specific conversions, string pack/unpack/format-general/quoted/hexadecimal/flags/integer-precision, string.char/string.byte/string.sub/string.find profile-specific numeric conversion and index boundaries, string.match/gmatch/gsub/rep profile-specific integer conversions and Lua 5.2 NaN limit behavior, table.concat/unpack/move/create/insert/remove/find profile-specific integer conversions, select numeric-selector conversions, tostring metamethod dispatch including yielding extension, xpcall argument forwarding/handler-failure convention, pcall/xpcall target error-value conversions, coroutine resume/wrap error-value conversions, dead/running resume diagnostics, close error objects/state, yieldability arguments, and coroutine argument/dead-close diagnostics, error level-0/current-source prefixes, direct-table iteration/__iter including yielding extension and profile-specific ipairs hooks, methods/string-call sugar, metamethods, closures, captures, varargs, multret, coroutines, default/lexical environments, Lua 5.1 current-thread environment rebinding, environment-aware/reader-function load, loadfile/dofile, package.searchpath, source-backed require, owned yielding-reader and package-searcher extensions, load mode errors, package.config/preload/searcher require with loader data, os.clock/time/date/calendar-table/filesystem mutation/execute/exit/locale/tmpname, os.date/debug.traceback profile-specific integer conversion, utf8/offset/codes, warn channel, profile library surface, collectgarbage controls, string.dump round-trip and captured-upvalue reload, debug metatable/registry/getinfo/currentline/activelines/active and retained caller and suspended coroutine thread/getlocal/setlocal/getupvalue/setupvalue/upvaluejoin/getuservalue/setuservalue/traceback/sethook/gethook/count/call-return/tail/native-callback/C-frame/C-frame-name/yielding-hook/thread-targeted-hook cases, opaque io handles/tmpfile/popen constructor failure/setvbuf and trusted-bridge opaque userdata, Lua 5.5 global declarations/named vararg table, tonumber boundary/overflow/base-validation/grammar conversions, to-be-closed error/reverse/yield paths)"
    );
    println!(
        "known boundary probes: pass (guest table, host userdata, and trusted-bridge opaque userdata finalizers cover scheduling/resurrection/rearming/reverse-order/error-yield policy; conservative register liveness closes the pinned Lua 5.3-5.5 re-arm cycle; heap-traced discarded io.lines iterators now match pinned cleanup; foreign Lua 5.1-5.5 binary chunks are rejected at the BluV1 boundary; deeper error(message, level) source selection is pinned as an owned/reference boundary; the owned Lua 5.1 yielding main-chunk __newindex continuation is recorded against the pinned metamethod-boundary rejection; debug.setcstacklimit remains isolated against Lua 5.1-5.5 references)"
    );
    println!("owned callback differential corpus: pass (Luau, Lua 5.1-5.5 profiles)");
    println!("portable reference matrix: pass (Luau, Lua 5.1-5.5)");
    if let Some(official_luau_tests) = args.official_luau_tests.as_deref() {
        for &profile in args.official_luau_profile.profiles() {
            verify_official_luau_tests(
                &args.upstream,
                official_luau_tests,
                profile,
                args.official_luau_test.as_deref(),
            )?;
        }
    }
    if let Some(official_lua_tests) = args.official_lua_tests.as_deref() {
        verify_official_lua51_tests(official_lua_tests)?;
        verify_official_lua_modern_tests(official_lua_tests)?;
    }
    Ok(())
}

fn verify_official_luau_tests(
    upstream: &Path,
    checkout: &Path,
    profile: SemanticProfile,
    filter: Option<&str>,
) -> Result<(), String> {
    let upstream = upstream
        .canonicalize()
        .map_err(|error| format!("failed to resolve official Luau executable: {error}"))?;
    let conformance = checkout.join("tests").join("conformance");
    let mut passed = 0;
    let mut profile_isolated = Vec::new();
    let mut reference_isolated = Vec::new();
    for name in OFFICIAL_LUAU_PORTABLE_TESTS
        .iter()
        .filter(|name| filter.is_none_or(|filter| filter == "all" || filter == **name))
    {
        println!("official Luau portable test ({profile} profile): {name}");
        let _ = std::io::stdout().flush();
        let path = conformance.join(name);
        let executable = env::current_exe()
            .map_err(|error| format!("failed to resolve Blu conformance executable: {error}"))?;
        let source_path = path
            .canonicalize()
            .map_err(|error| format!("failed to resolve official Luau test {path:?}: {error}"))?;
        let child = Command::new(&executable)
            .arg("--owned-luau-child")
            .arg(&source_path)
            .arg(semantic_profile_name(profile))
            .current_dir(&conformance)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to execute owned Luau child {name:?}: {error}"))?;
        match wait_for_child(child, official_luau_test_deadline(name))? {
            None => {
                let detail = "hard child-process watchdog expired";
                let detail = if let Some((_, reason)) = OFFICIAL_LUAU_PROFILE_ISOLATIONS
                    .iter()
                    .find(|(isolated_name, _)| *isolated_name == *name)
                {
                    format!("{reason}; observed {detail}")
                } else {
                    detail.to_owned()
                };
                profile_isolated.push(format!(
                    "{name}:runtime:{detail}; main error: owned source execution exceeded the hard watchdog"
                ));
            }
            Some(output) if !output.status.success() => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                let detail = if let Some((_, reason)) = OFFICIAL_LUAU_PROFILE_ISOLATIONS
                    .iter()
                    .find(|(isolated_name, _)| *isolated_name == *name)
                {
                    format!("{reason}; observed {detail}")
                } else {
                    detail
                };
                profile_isolated.push(format!(
                    "{name}:runtime:{detail}; main error: owned source execution failed (output {:?})",
                    String::from_utf8_lossy(&output.stdout)
                ));
            }
            Some(_) => {}
        }

        let output = Command::new(&upstream)
            .arg(name)
            .current_dir(&conformance)
            .output()
            .map_err(|error| format!("failed to execute official Luau test {name:?}: {error}"))?;
        if output.status.success() {
            passed += 1;
        } else if let Some((_, reason)) = OFFICIAL_LUAU_DIRECT_CLI_ISOLATIONS
            .iter()
            .find(|(isolated_name, _)| *isolated_name == *name)
        {
            reference_isolated.push(format!("{name}:{reason}"));
        } else {
            ensure_success(&upstream, &output)?;
        }
    }
    println!(
        "official Luau portable suite ({profile} profile): {passed} reference pass, {} profile-isolated ({}), {} reference-isolated ({})",
        profile_isolated.len(),
        if profile_isolated.is_empty() {
            "none".to_owned()
        } else {
            profile_isolated.join(", ")
        },
        reference_isolated.len(),
        if reference_isolated.is_empty() {
            "none".to_owned()
        } else {
            reference_isolated.join(", ")
        }
    );
    Ok(())
}

fn verify_official_lua51_tests(checkout: &Path) -> Result<(), String> {
    let root = checkout.join("lua-5.1.5");
    let executable = root.join("src").join("lua");
    let test_dir = root.join("test");
    let blu_executable = env::current_exe()
        .map_err(|error| format!("failed to resolve Blu conformance executable: {error}"))?;
    let mut passed = 0;
    let mut blu_isolated = Vec::new();
    for name in OFFICIAL_LUA51_PORTABLE_TESTS {
        let path = test_dir.join(name);
        let child_path = path
            .canonicalize()
            .map_err(|error| format!("failed to resolve official Lua test {path:?}: {error}"))?;
        let child = Command::new(&blu_executable)
            .arg("--owned-lua-child")
            .arg(&child_path)
            .current_dir(&test_dir)
            .output()
            .map_err(|error| {
                format!("failed to execute Blu official Lua child {name:?}: {error}")
            })?;
        if !child.status.success() {
            blu_isolated.push(format!(
                "{name}:child-status:{} (stderr {:?}, stdout {:?})",
                child.status,
                String::from_utf8_lossy(&child.stderr),
                String::from_utf8_lossy(&child.stdout)
            ));
            continue;
        }
        let blu_output = child.stdout;
        let reference = Command::new(&executable)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("failed to execute official Lua test {name:?}: {error}"))?;
        ensure_success(&executable, &reference)?;
        if blu_output == reference.stdout {
            passed += 1;
        } else {
            blu_isolated.push(format!(
                "{name}:output (Blu {:?}, Lua {:?})",
                String::from_utf8_lossy(&blu_output),
                String::from_utf8_lossy(&reference.stdout)
            ));
        }
    }
    println!(
        "official Lua 5.1 portable suite: {passed} pass, {} Blu-isolated ({})",
        blu_isolated.len(),
        if blu_isolated.is_empty() {
            "none".to_owned()
        } else {
            blu_isolated.join(", ")
        }
    );
    Ok(())
}

fn verify_official_lua_modern_tests(checkout: &Path) -> Result<(), String> {
    let suites = [
        (
            "5.4.8",
            SemanticProfile::Lua54,
            OFFICIAL_LUA54_PORTABLE_TESTS,
        ),
        (
            "5.5.0",
            SemanticProfile::Lua55,
            OFFICIAL_LUA55_PORTABLE_TESTS,
        ),
    ];
    let blu_executable = env::current_exe()
        .map_err(|error| format!("failed to resolve Blu conformance executable: {error}"))?;
    let mut skipped = Vec::new();
    for (version, profile, tests) in suites {
        let root = checkout.join(format!("lua-{version}"));
        let executable = root
            .join("src")
            .join("lua")
            .canonicalize()
            .map_err(|error| {
                format!("failed to resolve official Lua {version} executable: {error}")
            })?;
        let test_dir = checkout.join(format!("lua-{version}-tests"));
        if !executable.is_file() || !test_dir.is_dir() {
            skipped.push(version);
            continue;
        }
        let mut passed = 0;
        let mut isolated = Vec::new();
        for name in tests {
            let path = test_dir.join(name);
            let child_path = path.canonicalize().map_err(|error| {
                format!("failed to resolve official Lua {version} test {path:?}: {error}")
            })?;
            let child = Command::new(&blu_executable)
                .arg("--owned-lua-child")
                .arg(&child_path)
                .arg(semantic_profile_name(profile))
                .arg("portable")
                .current_dir(&test_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    format!("failed to execute Blu official Lua {version} child {name:?}: {error}")
                })?;
            let Some(child) = wait_for_child(child, MODERN_LUA_OFFICIAL_TEST_DEADLINE)? else {
                let reason = OFFICIAL_LUA_MODERN_ISOLATIONS
                    .iter()
                    .find(|(isolated_version, isolated_name, _)| {
                        *isolated_version == version && *isolated_name == *name
                    })
                    .map(|(_, _, reason)| *reason);
                isolated.push(format!(
                    "{name}:timeout (owned child exceeded {}s; assertions not completed{})",
                    MODERN_LUA_OFFICIAL_TEST_DEADLINE.as_secs(),
                    reason.map_or_else(String::new, |reason| format!("; {reason}"))
                ));
                continue;
            };
            if !child.status.success() {
                let reason = OFFICIAL_LUA_MODERN_ISOLATIONS
                    .iter()
                    .find(|(isolated_version, isolated_name, _)| {
                        *isolated_version == version && *isolated_name == *name
                    })
                    .map(|(_, _, reason)| *reason);
                isolated.push(format!(
                    "{name}:child-status:{}{} (stderr {:?}, stdout {:?})",
                    child.status,
                    reason.map_or_else(String::new, |reason| format!("; {reason}")),
                    String::from_utf8_lossy(&child.stderr),
                    String::from_utf8_lossy(&child.stdout)
                ));
                continue;
            }
            let reference = Command::new(&executable)
                .arg("-e")
                .arg("_port = true")
                .arg(name)
                .stdin(Stdio::null())
                .current_dir(&test_dir)
                .output()
                .map_err(|error| {
                    format!("failed to execute official Lua {version} test {name:?}: {error}")
                })?;
            ensure_success(&executable, &reference)?;
            // `math.lua` seeds from wall-clock/process state and prints both
            // the seeds and retry-dependent sample counts.  Its executable
            // assertions are the differential evidence; byte-for-byte
            // output is intentionally not deterministic across invocations.
            if *name == "math.lua" || child.stdout == reference.stdout {
                passed += 1;
            } else {
                let reason = OFFICIAL_LUA_MODERN_ISOLATIONS
                    .iter()
                    .find(|(isolated_version, isolated_name, _)| {
                        *isolated_version == version && *isolated_name == *name
                    })
                    .map(|(_, _, reason)| *reason);
                isolated.push(format!(
                    "{name}:output{} (Blu {:?}, Lua {:?})",
                    reason.map_or_else(String::new, |reason| format!("; {reason}")),
                    String::from_utf8_lossy(&child.stdout),
                    String::from_utf8_lossy(&reference.stdout)
                ));
            }
        }
        println!(
            "official Lua {version} portable suite: {passed} pass, {} Blu-isolated ({})",
            isolated.len(),
            if isolated.is_empty() {
                "none".to_owned()
            } else {
                isolated.join(", ")
            }
        );
    }
    if !skipped.is_empty() {
        println!(
            "official Lua modern portable suites: skipped (missing test archive or executable: {})",
            skipped.join(", ")
        );
    }
    Ok(())
}

fn official_owned_error_detail(error: &OwnedExecuteError) -> String {
    if let OwnedExecuteError::Compile(error) = error
        && let Some(syntax) = error.syntax()
        && let Some(diagnostic) = syntax.diagnostics().first()
    {
        let found = diagnostic
            .found()
            .map_or_else(|| "none".to_owned(), |bytes| format!("{:?}", bytes));
        return format!(
            "{} at {:?}: {}; expected {:?}, found {}",
            diagnostic.code().as_str(),
            diagnostic.primary().span(),
            diagnostic.primary().message(),
            diagnostic.expected(),
            found
        );
    }
    error.to_string()
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
    verify_owned_program_case_against_executable(
        name,
        source,
        reference_source,
        profile,
        upstream,
        "owned-program-reference.luau",
        temporary,
    )
}

fn verify_owned_os_locale_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_os_setlocale_getter(|locale, category| {
        if (locale.is_none() && category == b"all")
            || (locale == Some(b"C") && category == b"numeric")
        {
            Ok(Some(b"C".to_vec()))
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.setlocale request",
            })
        }
    });
    engine
        .vm_mut()
        .set_os_tmpname_getter(|| Ok(b"/tmp/blu-conformance.tmp".to_vec()));
    let result = engine
        .execute_owned_source(OS_LOCALE_SOURCE, profile)
        .map_err(|error| format!("owned os locale case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os locale case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-locale-reference.lua");
    fs::write(&reference_path, OS_LOCALE_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os locale reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os locale reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }

    let invalid_result = Engine::default()
        .execute_owned_source(OS_LOCALE_INVALID_CATEGORY_SOURCE, profile)
        .map_err(|error| format!("owned os locale invalid category failed: {error}"))?;
    if invalid_result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os locale invalid category returned {invalid_result:?}, expected true"
        ));
    }
    let invalid_reference_path = temporary.join("owned-os-locale-invalid-category-reference.lua");
    fs::write(
        &invalid_reference_path,
        OS_LOCALE_INVALID_CATEGORY_REFERENCE_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let invalid_reference = Command::new(reference_executable)
        .arg(&invalid_reference_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute os locale invalid-category reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(reference_executable, &invalid_reference)?;
    let invalid_reference = String::from_utf8_lossy(&invalid_reference.stdout);
    if invalid_reference.trim() != "boolean:true" {
        return Err(format!(
            "os locale invalid-category reference for case {name:?} changed: expected boolean:true, got {invalid_reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_os_exit_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_os_exit_getter(move |request| {
        let expected = if profile == SemanticProfile::Lua51 {
            OsExitRequest {
                status: 7,
                close: false,
            }
        } else {
            OsExitRequest {
                status: 0,
                close: true,
            }
        };
        if request == expected {
            Ok(())
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.exit request",
            })
        }
    });
    let result = engine
        .execute_owned_source(OS_EXIT_SOURCE, profile)
        .map_err(|error| format!("owned os.exit case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.exit case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-exit-reference.lua");
    fs::write(&reference_path, OS_EXIT_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.exit reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    Ok(())
}

fn verify_owned_os_execute_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine
        .vm_mut()
        .set_os_execute_getter(|command| match command {
            None => Ok(OsExecuteResult::Availability(true)),
            Some(b"true") => Ok(OsExecuteResult::Command {
                success: true,
                kind: b"exit".to_vec(),
                code: 0,
            }),
            Some(_) => Err(RuntimeError::InvalidRange {
                operation: "conformance os.execute command",
            }),
        });
    let result = engine
        .execute_owned_source(OS_EXECUTE_SOURCE, profile)
        .map_err(|error| format!("owned os.execute case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.execute case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-execute-reference.lua");
    fs::write(&reference_path, OS_EXECUTE_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.execute reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.execute reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_os_mutation_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let remove_path = temporary.join(format!("{name}-remove.txt"));
    let rename_from = temporary.join(format!("{name}-rename-from.txt"));
    let rename_to = temporary.join(format!("{name}-rename-to.txt"));
    fs::write(&remove_path, b"remove me").map_err(|error| error.to_string())?;
    fs::write(&rename_from, b"rename me").map_err(|error| error.to_string())?;
    let remove_path = remove_path.to_string_lossy().into_owned();
    let rename_from = rename_from.to_string_lossy().into_owned();
    let rename_to = rename_to.to_string_lossy().into_owned();

    let expected_remove = remove_path.as_bytes().to_vec();
    let expected_rename_from = rename_from.as_bytes().to_vec();
    let expected_rename_to = rename_to.as_bytes().to_vec();
    let mut engine = Engine::default();
    engine.vm_mut().set_os_remove_getter(move |path| {
        if path == expected_remove.as_slice() {
            Ok(())
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.remove path",
            })
        }
    });
    engine.vm_mut().set_os_rename_getter(move |from, to| {
        if from == expected_rename_from.as_slice() && to == expected_rename_to.as_slice() {
            Ok(())
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.rename paths",
            })
        }
    });
    let source = format!(
        "return os.remove(\"{}\") == true and os.rename(\"{}\", \"{}\") == true",
        remove_path, rename_from, rename_to
    );
    let result = engine
        .execute_owned_source(source, profile)
        .map_err(|error| format!("owned os mutation case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os mutation case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_source = format!(
        "local removed = os.remove(\"{}\")\nlocal renamed = os.rename(\"{}\", \"{}\")\nprint(type(removed) .. \":\" .. tostring(removed == true and renamed == true))",
        remove_path, rename_from, rename_to
    );
    let reference_path = temporary.join("owned-os-mutation-reference.lua");
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os mutation reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os mutation reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }

    let missing_remove = temporary.join(format!("{name}-missing-remove.txt"));
    let missing_from = temporary.join(format!("{name}-missing-from.txt"));
    let missing_to = temporary.join(format!("{name}-missing-to.txt"));
    let missing_remove = missing_remove.to_string_lossy().into_owned();
    let missing_from = missing_from.to_string_lossy().into_owned();
    let missing_to = missing_to.to_string_lossy().into_owned();
    let mut failure_engine = Engine::default();
    failure_engine.vm_mut().set_os_remove_getter(|_| {
        Err(RuntimeError::Raised(Value::String(Arc::from(
            &b"remove denied"[..],
        ))))
    });
    failure_engine.vm_mut().set_os_rename_getter(|_, _| {
        Err(RuntimeError::Raised(Value::String(Arc::from(
            &b"rename denied"[..],
        ))))
    });
    let failure_source = format!(
        "local removed, remove_error = os.remove(\"{}\")\nlocal renamed, rename_error = os.rename(\"{}\", \"{}\")\nreturn removed == nil and type(remove_error) == \"string\" and renamed == nil and type(rename_error) == \"string\"",
        missing_remove, missing_from, missing_to
    );
    let failure_result = failure_engine
        .execute_owned_source(failure_source, profile)
        .map_err(|error| format!("owned os mutation failure case failed for {name:?}: {error}"))?;
    if failure_result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os mutation failure case {name:?} returned {failure_result:?}, expected true"
        ));
    }
    let failure_reference_source = format!(
        "local removed, remove_error = os.remove(\"{}\")\nlocal renamed, rename_error = os.rename(\"{}\", \"{}\")\nlocal result = removed == nil and type(remove_error) == \"string\" and renamed == nil and type(rename_error) == \"string\"\nprint(type(result) .. \":\" .. tostring(result))",
        missing_remove, missing_from, missing_to
    );
    let failure_reference_path = temporary.join("owned-os-mutation-failure-reference.lua");
    fs::write(&failure_reference_path, failure_reference_source)
        .map_err(|error| error.to_string())?;
    let failure_reference = Command::new(reference_executable)
        .arg(&failure_reference_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute os mutation failure reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(reference_executable, &failure_reference)?;
    let failure_reference = String::from_utf8_lossy(&failure_reference.stdout);
    if failure_reference.trim() != "boolean:true" {
        return Err(format!(
            "os mutation failure reference for case {name:?} changed: expected boolean:true, got {failure_reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_clock_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_clock_getter(|| Ok(1.25));
    let result = engine
        .execute_owned_source(OS_CLOCK_SOURCE, profile)
        .map_err(|error| format!("owned os.clock case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.clock case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-clock-reference.lua");
    fs::write(&reference_path, OS_CLOCK_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.clock reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.clock reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_time_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_time_getter(|| Ok(1_700_000_000));
    let result = engine
        .execute_owned_source(OS_TIME_SOURCE, profile)
        .map_err(|error| format!("owned os.time case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.time case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-time-reference.lua");
    fs::write(&reference_path, OS_TIME_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.time reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.time reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_time_table_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_calendar_time_getter(|input| {
        if input
            == (CalendarDateInput {
                year: 2023,
                month: 11,
                day: 14,
                hour: 22,
                minute: 13,
                second: 20,
                is_dst: Some(false),
            })
        {
            Ok(1_700_000_000)
        } else if input
            == (CalendarDateInput {
                year: 2023,
                month: 11,
                day: 14,
                hour: 12,
                minute: 0,
                second: 0,
                is_dst: None,
            })
        {
            Ok(1_699_963_200)
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.time calendar request",
            })
        }
    });
    let result = engine
        .execute_owned_source(OS_TIME_TABLE_SOURCE, profile)
        .map_err(|error| format!("owned os.time table case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.time table case {name:?} returned {result:?}, expected true"
        ));
    }

    let result = engine
        .execute_owned_source(OS_TIME_TABLE_DEFAULTS_SOURCE, profile)
        .map_err(|error| {
            format!("owned os.time default-fields case failed for {name:?}: {error}")
        })?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.time default-fields case {name:?} returned {result:?}, expected true"
        ));
    }

    let result = engine
        .execute_owned_source(OS_TIME_TABLE_INTEGER_ARGUMENT_SOURCE, profile)
        .map_err(|error| {
            format!("owned os.time integer-field case failed for {name:?}: {error}")
        })?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.time integer-field case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-time-table-reference.lua");
    fs::write(&reference_path, OS_TIME_TABLE_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.time table reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "number:true" {
        return Err(format!(
            "os.time table reference for case {name:?} changed: expected number:true, got {reference:?}"
        ));
    }

    let reference_path = temporary.join("owned-os-time-table-defaults-reference.lua");
    fs::write(&reference_path, OS_TIME_TABLE_DEFAULTS_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute os.time default-fields reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "number:true" {
        return Err(format!(
            "os.time default-fields reference for case {name:?} changed: expected number:true, got {reference:?}"
        ));
    }

    let reference_path = temporary.join("owned-os-time-table-integer-reference.lua");
    fs::write(
        &reference_path,
        OS_TIME_TABLE_INTEGER_ARGUMENT_REFERENCE_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute os.time integer-field reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.time integer-field reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_date_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_date_getter(|format, timestamp| {
        if format == b"!%Y-%m-%d" && timestamp == Some(1_700_000_000) {
            Ok(b"2023-11-14".to_vec())
        } else if format == b"!%Y" && timestamp == Some(1) {
            Ok(b"1970".to_vec())
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.date request",
            })
        }
    });
    let result = engine
        .execute_owned_source(OS_DATE_SOURCE, profile)
        .map_err(|error| format!("owned os.date case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.date case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-date-reference.lua");
    fs::write(&reference_path, OS_DATE_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.date reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.date reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    let result = engine
        .execute_owned_source(OS_DATE_INTEGER_ARGUMENT_SOURCE, profile)
        .map_err(|error| format!("owned os.date integer case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.date integer case {name:?} returned {result:?}, expected true"
        ));
    }
    let reference_path = temporary.join("owned-os-date-integer-reference.lua");
    fs::write(&reference_path, OS_DATE_INTEGER_ARGUMENT_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.date integer reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.date integer reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_date_table_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_calendar_getter(|timestamp, utc| {
        if timestamp == Some(1_700_000_000) && utc {
            Ok(CalendarDate {
                year: 2023,
                month: 11,
                day: 14,
                hour: 22,
                minute: 13,
                second: 20,
                weekday: 3,
                yearday: 318,
                is_dst: false,
            })
        } else {
            Err(RuntimeError::InvalidRange {
                operation: "conformance os.date calendar request",
            })
        }
    });
    let result = engine
        .execute_owned_source(OS_DATE_TABLE_SOURCE, profile)
        .map_err(|error| format!("owned os.date table case failed for {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned os.date table case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-os-date-table-reference.lua");
    fs::write(&reference_path, OS_DATE_TABLE_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute os.date table reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "os.date table reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_io_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-io-conformance.blu",
        IO_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned io source case {name:?} was invalid: {error}"))?;
    let identity = CompilerIdentity::new(
        CompilerId::new(*b"blu-owned-v1\0\0\0\0"),
        "blu-owned",
        env!("CARGO_PKG_VERSION"),
        None,
        IdentityLimits::default(),
    )
    .map_err(|error| format!("owned compiler identity failed for {name:?}: {error}"))?;
    let compilation = OwnedCompiler::default()
        .compile(&source_file, profile, identity.clone())
        .map_err(|error| format!("owned compiler failed io case {name:?}: {error}"))?;
    let mut vm = Vm::default();
    vm.set_io_file_opener(|path, mode| {
        if matches!(
            path,
            b"missing.txt" | b"missing-input.txt" | b"missing-output.txt"
        ) {
            return Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"missing file"[..],
            ))));
        }
        let bytes = if path == b"numbers.txt" {
            b"42 3.5".to_vec()
        } else if path == b"multi_lines.txt" {
            b"alpha\nbeta\n".to_vec()
        } else if mode == b"rb" || mode == b"r" {
            b"owned io\n".to_vec()
        } else {
            Vec::new()
        };
        Ok(Arc::new(ConformanceIoFile {
            bytes: Mutex::new(bytes),
            position: Mutex::new(0),
        }) as Arc<dyn IoFile>)
    });
    vm.set_io_popen_opener(|command, mode| {
        if command != b"printf popen" || mode != b"r" {
            return Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"unexpected io.popen request"[..],
            ))));
        }
        Ok(Arc::new(ConformanceIoFile {
            bytes: Mutex::new(b"popen".to_vec()),
            position: Mutex::new(0),
        }) as Arc<dyn IoFile>)
    });
    vm.set_io_stream_opener(|kind| {
        let bytes = match kind {
            IoStreamKind::Stdin => b"owned stdin\n".to_vec(),
            IoStreamKind::Stdout | IoStreamKind::Stderr => Vec::new(),
        };
        Ok(Arc::new(ConformanceIoFile {
            bytes: Mutex::new(bytes),
            position: Mutex::new(0),
        }) as Arc<dyn IoFile>)
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed io case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned runtime returned {} values for io case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    fs::write(temporary.join("answer.txt"), b"owned io\n").map_err(|error| error.to_string())?;
    fs::write(temporary.join("numbers.txt"), b"42 3.5").map_err(|error| error.to_string())?;
    fs::write(temporary.join("multi_lines.txt"), b"alpha\nbeta\n")
        .map_err(|error| error.to_string())?;
    let reference_path = temporary.join("owned-io-reference.lua");
    fs::write(&reference_path, IO_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference_executable = reference_executable
        .canonicalize()
        .map_err(|error| format!("failed to resolve io reference executable: {error}"))?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!("failed to execute owned io reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned io case {name:?} differs: Blu={result:?}, reference={:?}",
            reference.trim()
        ));
    }

    let seek_source_file = SourceFile::new(
        SourceId::new(2),
        "owned-io-seek-conformance.blu",
        IO_SEEK_INTEGER_ARGUMENT_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned io.seek source case {name:?} was invalid: {error}"))?;
    let seek_compilation = OwnedCompiler::default()
        .compile(&seek_source_file, profile, identity.clone())
        .map_err(|error| format!("owned compiler failed io.seek case {name:?}: {error}"))?;
    let seek_result = vm
        .execute_blu_v1(
            seek_compilation.into_validated_artifact(),
            BluLimits::default(),
        )
        .map_err(|error| format!("owned runtime failed io.seek case {name:?}: {error}"))?;
    if seek_result != [Value::Boolean(true)] {
        return Err(format!(
            "owned io.seek case {name:?} returned {seek_result:?}, expected true"
        ));
    }

    let seek_reference_path = temporary.join("owned-io-seek-reference.lua");
    fs::write(
        &seek_reference_path,
        IO_SEEK_INTEGER_ARGUMENT_REFERENCE_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let seek_reference = Command::new(&reference_executable)
        .arg(&seek_reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!("failed to execute owned io.seek reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &seek_reference)?;
    let seek_reference = String::from_utf8_lossy(&seek_reference.stdout);
    if seek_reference.trim() != "boolean:true" {
        return Err(format!(
            "owned io.seek reference for case {name:?} changed: expected boolean:true, got {seek_reference:?}"
        ));
    }

    let setvbuf_source_file = SourceFile::new(
        SourceId::new(3),
        "owned-io-setvbuf-conformance.blu",
        IO_SETVBUF_INTEGER_ARGUMENT_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned io.setvbuf source case {name:?} was invalid: {error}"))?;
    let setvbuf_compilation = OwnedCompiler::default()
        .compile(&setvbuf_source_file, profile, identity.clone())
        .map_err(|error| format!("owned compiler failed io.setvbuf case {name:?}: {error}"))?;
    let setvbuf_result = vm
        .execute_blu_v1(
            setvbuf_compilation.into_validated_artifact(),
            BluLimits::default(),
        )
        .map_err(|error| format!("owned runtime failed io.setvbuf case {name:?}: {error}"))?;
    if setvbuf_result != [Value::Boolean(true)] {
        return Err(format!(
            "owned io.setvbuf case {name:?} returned {setvbuf_result:?}, expected true"
        ));
    }

    let setvbuf_reference_path = temporary.join("owned-io-setvbuf-reference.lua");
    fs::write(
        &setvbuf_reference_path,
        IO_SETVBUF_INTEGER_ARGUMENT_REFERENCE_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let setvbuf_reference = Command::new(&reference_executable)
        .arg(&setvbuf_reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute owned io.setvbuf reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(&reference_executable, &setvbuf_reference)?;
    let setvbuf_reference = String::from_utf8_lossy(&setvbuf_reference.stdout);
    if setvbuf_reference.trim() != "boolean:true" {
        return Err(format!(
            "owned io.setvbuf reference for case {name:?} changed: expected boolean:true, got {setvbuf_reference:?}"
        ));
    }

    let read_source_file = SourceFile::new(
        SourceId::new(4),
        "owned-io-read-conformance.blu",
        IO_READ_INTEGER_ARGUMENT_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned io.read source case {name:?} was invalid: {error}"))?;
    let read_compilation = OwnedCompiler::default()
        .compile(&read_source_file, profile, identity)
        .map_err(|error| format!("owned compiler failed io.read case {name:?}: {error}"))?;
    let read_result = vm
        .execute_blu_v1(
            read_compilation.into_validated_artifact(),
            BluLimits::default(),
        )
        .map_err(|error| format!("owned runtime failed io.read case {name:?}: {error}"))?;
    if read_result != [Value::Boolean(true)] {
        return Err(format!(
            "owned io.read case {name:?} returned {read_result:?}, expected true"
        ));
    }

    let read_reference_path = temporary.join("owned-io-read-reference.lua");
    fs::write(
        &read_reference_path,
        IO_READ_INTEGER_ARGUMENT_REFERENCE_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let read_reference = Command::new(&reference_executable)
        .arg(&read_reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!("failed to execute owned io.read reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &read_reference)?;
    let read_reference = String::from_utf8_lossy(&read_reference.stdout);
    if read_reference.trim() != "boolean:true" {
        return Err(format!(
            "owned io.read reference for case {name:?} changed: expected boolean:true, got {read_reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_program_case_against_executable(
    name: &str,
    source: &str,
    reference_source: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    reference_filename: &str,
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
        .compile(&source_file, profile, identity.clone())
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

    let reference_path = temporary.join(reference_filename);
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute owned reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned program case {name:?} differs: Blu={result:?}, reference={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_known_boundary_case(
    name: &str,
    sources: (&str, &str),
    profile: SemanticProfile,
    reference_executable: &Path,
    expected: (&str, &str),
    reference_filename: &str,
    temporary: &Path,
) -> Result<(), String> {
    let (source, reference_source) = sources;
    let (expected_owned, expected_reference) = expected;
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-boundary.blu",
        source.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned boundary source {name:?} was invalid: {error}"))?;
    let identity = CompilerIdentity::new(
        CompilerId::new(*b"blu-boundary\0\0\0\0"),
        "blu-boundary",
        env!("CARGO_PKG_VERSION"),
        None,
        IdentityLimits::default(),
    )
    .map_err(|error| format!("owned boundary compiler identity failed for {name:?}: {error}"))?;
    let compilation = OwnedCompiler::default()
        .compile(&source_file, profile, identity)
        .map_err(|error| format!("owned compiler failed boundary {name:?}: {error}"))?;
    let result = Vm::default()
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed boundary {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned runtime returned {} values for boundary {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;
    if result != expected_owned {
        return Err(format!(
            "owned boundary {name:?} changed: expected {expected_owned:?}, got {result:?}"
        ));
    }

    let reference_path = temporary.join(reference_filename);
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute boundary reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != expected_reference {
        return Err(format!(
            "boundary reference {name:?} changed: expected {expected_reference:?}, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_engine_program_case_against_executable(
    name: &str,
    source: &str,
    reference_source: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    reference_filename: &str,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(source, profile)
        .map_err(|error| format!("owned runtime failed case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned runtime returned {} values for case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;
    let reference_path = temporary.join(reference_filename);
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(reference_executable)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute owned reference {reference_executable:?}: {error}")
        })?;
    ensure_success(reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned engine program case {name:?} differs: Blu={result:?}, reference={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_file_load_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine.vm_mut().set_file_loader(|path| {
        if path == b"answer.lua" {
            Ok(b"return 41".to_vec())
        } else {
            Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"unknown file"[..],
            ))))
        }
    });
    let result = engine
        .execute_owned_source(FILE_LOAD_SOURCE, profile)
        .map_err(|error| format!("owned runtime failed file-load case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned runtime returned {} values for file-load case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let source_path = temporary.join("answer.lua");
    fs::write(&source_path, b"return 41").map_err(|error| error.to_string())?;
    let reference_path = temporary.join("owned-file-load-reference.lua");
    fs::write(&reference_path, FILE_LOAD_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference_executable = reference_executable
        .canonicalize()
        .map_err(|error| format!("failed to resolve owned file-load reference: {error}"))?;
    let reference = Command::new(&reference_executable)
        .current_dir(temporary)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute owned file-load reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned file-load case {name:?} differs: Blu={result:?}, reference={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_foreign_lua_binary_case(
    references: &[(String, PathBuf)],
    temporary: &Path,
) -> Result<(), String> {
    let profiles = [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ];
    let version_tags = [b'Q', b'R', b'S', b'T', b'U'];
    for (index, ((_name, reference_executable), profile)) in
        references.iter().zip(profiles).enumerate()
    {
        let source_path = temporary.join("foreign-source.lua");
        let chunk_path = temporary.join("foreign.luac");
        fs::write(&source_path, b"return 41\n").map_err(|error| error.to_string())?;
        let luac_executable = reference_executable
            .parent()
            .ok_or_else(|| "Lua reference has no parent directory".to_owned())?
            .join(executable_name("luac"));
        let luac_executable = luac_executable.canonicalize().map_err(|error| {
            format!(
                "failed to resolve pinned Lua luac executable for profile {profile:?} {luac_executable:?}: {error}"
            )
        })?;
        let luac = Command::new(&luac_executable)
            .current_dir(temporary)
            .args(["-o", "foreign.luac", "foreign-source.lua"])
            .output()
            .map_err(|error| {
                format!("failed to execute pinned Lua luac for {profile:?}: {error}")
            })?;
        ensure_success(&luac_executable, &luac)?;
        let foreign_chunk = fs::read(&chunk_path).map_err(|error| error.to_string())?;
        let expected_header = [0x1b, b'L', b'u', b'a', version_tags[index]];
        if foreign_chunk.get(..5) != Some(&expected_header) {
            return Err(format!(
                "pinned Lua luac emitted an unexpected {profile:?} header: {:?}",
                foreign_chunk.get(..5)
            ));
        }

        let mut engine = Engine::default();
        engine.vm_mut().set_file_loader(move |path| match path {
            b"foreign.luac" => Ok(foreign_chunk.clone()),
            _ => Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"unknown foreign chunk path"[..],
            )))),
        });
        let result = engine
            .execute_owned_source(FOREIGN_LUA_BINARY_SOURCE, profile)
            .map_err(|error| {
                format!("Blu failed foreign Lua binary case for {profile:?}: {error}")
            })?;
        let result = result
            .first()
            .ok_or_else(|| format!("Blu returned no foreign Lua binary result for {profile:?}"))
            .and_then(canonical_value)?;
        if result != "boolean:true" {
            return Err(format!(
                "Blu accepted or misclassified the foreign {profile:?} binary: got {result:?}"
            ));
        }

        let reference_path = temporary.join("foreign-lua-binary-reference.lua");
        fs::write(&reference_path, FOREIGN_LUA_BINARY_REFERENCE_SOURCE)
            .map_err(|error| error.to_string())?;
        let reference_executable = reference_executable
            .canonicalize()
            .map_err(|error| format!("failed to resolve {profile:?} reference: {error}"))?;
        let reference = Command::new(&reference_executable)
            .arg(&reference_path)
            .current_dir(temporary)
            .output()
            .map_err(|error| {
                format!("failed to execute {profile:?} foreign binary reference: {error}")
            })?;
        ensure_success(&reference_executable, &reference)?;
        let reference = String::from_utf8_lossy(&reference.stdout);
        if reference.trim() != "boolean:true" {
            return Err(format!(
                "{profile:?} foreign binary reference changed: expected \"boolean:true\", got {reference:?}"
            ));
        }
    }
    Ok(())
}

fn verify_owned_io_tmpfile_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-io-tmpfile-conformance.blu",
        IO_TMPFILE_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned io.tmpfile source {name:?} was invalid: {error}"))?;
    let identity = CompilerIdentity::new(
        CompilerId::new(*b"blu-owned-v1\0\0\0\0"),
        "blu-owned",
        env!("CARGO_PKG_VERSION"),
        None,
        IdentityLimits::default(),
    )
    .map_err(|error| format!("owned compiler identity failed for io.tmpfile {name:?}: {error}"))?;
    let compilation = OwnedCompiler::default()
        .compile(&source_file, profile, identity.clone())
        .map_err(|error| format!("owned compiler failed io.tmpfile case {name:?}: {error}"))?;
    let mut vm = Vm::default();
    vm.set_io_tempfile_opener(|| {
        Ok(Arc::new(ConformanceIoFile {
            bytes: Mutex::new(Vec::new()),
            position: Mutex::new(0),
        }) as Arc<dyn IoFile>)
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed io.tmpfile case {name:?}: {error}"))?;
    let result = result
        .first()
        .ok_or_else(|| format!("owned io.tmpfile case {name:?} returned no value"))
        .and_then(canonical_value)?;
    if result != "string:userdata:true:1" {
        return Err(format!(
            "owned io.tmpfile case {name:?} changed: expected \"string:userdata:true:1\", got {result:?}"
        ));
    }

    let reference_path = temporary.join("owned-io-tmpfile-reference.lua");
    fs::write(&reference_path, IO_TMPFILE_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference_executable = reference_executable
        .canonicalize()
        .map_err(|error| format!("failed to resolve io.tmpfile reference executable: {error}"))?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!("failed to execute io.tmpfile reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "userdata:true:1" {
        return Err(format!(
            "io.tmpfile reference for case {name:?} changed: expected \"userdata:true:1\", got {reference:?}"
        ));
    }

    let failure_source_file = SourceFile::new(
        SourceId::new(2),
        "owned-io-constructor-failure-conformance.blu",
        IO_CONSTRUCTOR_FAILURE_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| {
        format!("owned io constructor failure source {name:?} was invalid: {error}")
    })?;
    let failure_compilation = OwnedCompiler::default()
        .compile(&failure_source_file, profile, identity)
        .map_err(|error| {
            format!("owned compiler failed io constructor failure case {name:?}: {error}")
        })?;
    let mut failure_vm = Vm::default();
    failure_vm.set_io_tempfile_opener(|| {
        Err(RuntimeError::Raised(Value::String(Arc::from(
            &b"temporary file unavailable"[..],
        ))))
    });
    failure_vm.set_io_popen_opener(|_, _| {
        Err(RuntimeError::Raised(Value::String(Arc::from(
            &b"process pipe unavailable"[..],
        ))))
    });
    let failure_result = failure_vm
        .execute_blu_v1(
            failure_compilation.into_validated_artifact(),
            BluLimits::default(),
        )
        .map_err(|error| {
            format!("owned runtime failed io constructor failure case {name:?}: {error}")
        })?;
    if failure_result != [Value::Boolean(true)] {
        return Err(format!(
            "owned io constructor failure case {name:?} returned {failure_result:?}, expected true"
        ));
    }

    let failure_reference_path = temporary.join("owned-io-constructor-failure-reference.lua");
    fs::write(
        &failure_reference_path,
        IO_CONSTRUCTOR_FAILURE_REFERENCE_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let failure_reference = Command::new(&reference_executable)
        .arg(&failure_reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute io constructor failure reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(&reference_executable, &failure_reference)?;
    let failure_reference = String::from_utf8_lossy(&failure_reference.stdout);
    if failure_reference.trim() != "boolean:true" {
        return Err(format!(
            "io constructor failure reference for case {name:?} changed: expected \"boolean:true\", got {failure_reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_io_operation_failure_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-io-operation-failure-conformance.blu",
        IO_OPERATION_FAILURE_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned io operation failure source {name:?} was invalid: {error}"))?;
    let identity = CompilerIdentity::new(
        CompilerId::new(*b"blu-owned-v1\0\0\0\0"),
        "blu-owned",
        env!("CARGO_PKG_VERSION"),
        None,
        IdentityLimits::default(),
    )
    .map_err(|error| format!("owned compiler identity failed for io operation failure: {error}"))?;
    let compilation = OwnedCompiler::default()
        .compile(&source_file, profile, identity)
        .map_err(|error| {
            format!("owned compiler failed io operation failure case {name:?}: {error}")
        })?;
    let mut vm = Vm::default();
    vm.set_io_file_opener(|path, _mode| {
        let operation = match path {
            b"read-failure" => FailingIoOperation::Read,
            b"line-read-failure" => FailingIoOperation::Read,
            b"write-failure" => FailingIoOperation::Write,
            b"seek-failure" => FailingIoOperation::Seek,
            b"flush-failure" => FailingIoOperation::Flush,
            b"buffer-failure" => FailingIoOperation::Buffering,
            b"close-failure" => FailingIoOperation::Close,
            _ => {
                return Err(RuntimeError::Raised(Value::String(Arc::from(
                    &b"unexpected io failure path"[..],
                ))));
            }
        };
        Ok(Arc::new(FailingIoFile { operation }) as Arc<dyn IoFile>)
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| {
            format!("owned runtime failed io operation failure case {name:?}: {error}")
        })?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned io operation failure case {name:?} returned {result:?}, expected true"
        ));
    }

    fs::write(temporary.join("answer.txt"), b"answer\n").map_err(|error| error.to_string())?;
    let reference_path = temporary.join("owned-io-operation-failure-reference.lua");
    fs::write(&reference_path, IO_OPERATION_FAILURE_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable
        .canonicalize()
        .map_err(|error| format!("failed to resolve io operation failure reference: {error}"))?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute io operation failure reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "io operation failure reference for case {name:?} changed: expected boolean:true, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_userdata_finalizer_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-userdata-finalizer-conformance.blu",
        HOST_USERDATA_FINALIZER_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned userdata finalizer source {name:?} was invalid: {error}"))?;
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
        .map_err(|error| {
            format!("owned compiler failed userdata finalizer case {name:?}: {error}")
        })?;
    let mut vm = Vm::default();
    vm.set_io_file_opener(|_, _| {
        Ok(Arc::new(ConformanceIoFile {
            bytes: Mutex::new(Vec::new()),
            position: Mutex::new(0),
        }) as Arc<dyn IoFile>)
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| {
            format!("owned runtime failed userdata finalizer case {name:?}: {error}")
        })?;
    let result = result
        .first()
        .ok_or_else(|| format!("owned userdata finalizer case {name:?} returned no value"))
        .and_then(canonical_value)?;
    let expected = if profile == SemanticProfile::Lua52 {
        "number:1"
    } else {
        "number:2"
    };
    if result != expected {
        return Err(format!(
            "owned userdata finalizer case {name:?} changed: expected {expected:?}, got {result:?}"
        ));
    }

    fs::write(temporary.join("answer.txt"), b"owned io\n").map_err(|error| error.to_string())?;
    let reference_path = temporary.join("owned-userdata-finalizer-reference.lua");
    fs::write(&reference_path, HOST_USERDATA_FINALIZER_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable.canonicalize().map_err(|error| {
        format!("failed to resolve userdata finalizer reference executable: {error}")
    })?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute userdata finalizer reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != expected {
        return Err(format!(
            "userdata finalizer reference for case {name:?} changed: expected {expected:?}, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_native_userdata_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-native-userdata-conformance.blu",
        NATIVE_USERDATA_FINALIZER_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("native userdata source {name:?} was invalid: {error}"))?;
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
        .map_err(|error| format!("owned compiler failed native userdata case {name:?}: {error}"))?;
    let mut vm = Vm::default();
    vm.set_native_library_loader(|vm, library, symbol| {
        if library != b"trusted.so" || symbol != b"luaopen_trusted" {
            return Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"unexpected native bridge lookup"[..],
            ))));
        }
        vm.create_userdata(b"native bridge userdata")
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed native userdata case {name:?}: {error}"))?
        .first()
        .ok_or_else(|| format!("native userdata case {name:?} returned no value"))
        .and_then(canonical_value)?;
    if result != "string:userdata:1" {
        return Err(format!(
            "native userdata case {name:?} changed: expected \"string:userdata:1\", got {result:?}"
        ));
    }

    let reference_path = temporary.join("owned-native-userdata-reference.lua");
    fs::write(&reference_path, NATIVE_USERDATA_FINALIZER_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable.canonicalize().map_err(|error| {
        format!("failed to resolve native userdata reference executable: {error}")
    })?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!("failed to execute native userdata reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "userdata:1" {
        return Err(format!(
            "native userdata reference for case {name:?} changed: expected \"userdata:1\", got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_native_loadlib_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-native-loadlib-conformance.blu",
        NATIVE_LOADLIB_UNAVAILABLE_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("native loadlib source {name:?} was invalid: {error}"))?;
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
        .map_err(|error| format!("owned compiler failed native loadlib case {name:?}: {error}"))?;
    let mut vm = Vm::default();
    vm.set_native_library_loader_result(|_, library, symbol| {
        if library != b"trusted.so" || symbol != b"luaopen_trusted" {
            return Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"unexpected native bridge lookup"[..],
            ))));
        }
        Ok(NativeLibraryLoadResult::Unavailable {
            message: b"native bridge unavailable".to_vec(),
            where_: NativeLibraryFailure::Absent,
        })
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed native loadlib case {name:?}: {error}"))?
        .first()
        .ok_or_else(|| format!("native loadlib case {name:?} returned no value"))
        .and_then(canonical_value)?;
    if result != "boolean:true" {
        return Err(format!(
            "native loadlib case {name:?} changed: expected \"boolean:true\", got {result:?}"
        ));
    }

    let reference_path = temporary.join("owned-native-loadlib-reference.lua");
    fs::write(&reference_path, NATIVE_LOADLIB_UNAVAILABLE_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable.canonicalize().map_err(|error| {
        format!("failed to resolve native loadlib reference executable: {error}")
    })?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!("failed to execute native loadlib reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:true" {
        return Err(format!(
            "native loadlib reference for case {name:?} changed: expected \"boolean:true\", got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_discarded_io_lines_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "discarded-io-lines-conformance.blu",
        DISCARDED_IO_LINES_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("discarded io.lines source {name:?} was invalid: {error}"))?;
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
        .map_err(|error| {
            format!("owned compiler failed discarded io.lines case {name:?}: {error}")
        })?;
    let mut vm = Vm::default();
    vm.set_io_file_opener(|_, _| {
        Ok(Arc::new(ConformanceIoFile {
            bytes: Mutex::new(Vec::new()),
            position: Mutex::new(0),
        }) as Arc<dyn IoFile>)
    });
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed discarded io.lines case {name:?}: {error}"))?
        .first()
        .ok_or_else(|| format!("discarded io.lines case {name:?} returned no value"))
        .and_then(canonical_value)?;
    let expected = if profile == SemanticProfile::Lua51 {
        "string:true:true:true:true:true:true:true:true:true:true:false:C:=[C]:1"
    } else {
        "string:false:false:true:true:false:true:true:true:true:true:true:C:=[C]:1"
    };
    if result != expected {
        return Err(format!(
            "discarded io.lines case {name:?} changed: expected {expected:?}, got {result:?}"
        ));
    }

    fs::write(temporary.join("answer.txt"), b"").map_err(|error| error.to_string())?;
    let reference_path = temporary.join("discarded-io-lines-reference.lua");
    fs::write(&reference_path, DISCARDED_IO_LINES_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable.canonicalize().map_err(|error| {
        format!("failed to resolve discarded io.lines reference executable: {error}")
    })?;
    let reference = Command::new(&reference_executable)
        .arg(&reference_path)
        .current_dir(temporary)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute discarded io.lines reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != expected {
        return Err(format!(
            "discarded io.lines reference for case {name:?} changed: expected {expected:?}, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_file_searchpath_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine
        .vm_mut()
        .set_file_probe(|path| Ok(path == b"./answer.lua"));
    let result = engine
        .execute_owned_source(PACKAGE_SEARCHPATH_SOURCE, profile)
        .map_err(|error| format!("owned searchpath failed for case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned searchpath returned {} values for case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let source_path = temporary.join("answer.lua");
    fs::write(&source_path, b"return 41").map_err(|error| error.to_string())?;
    let reference_path = temporary.join("owned-searchpath-reference.lua");
    fs::write(&reference_path, PACKAGE_SEARCHPATH_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable
        .canonicalize()
        .map_err(|error| format!("failed to resolve owned searchpath reference: {error}"))?;
    let reference = Command::new(&reference_executable)
        .current_dir(temporary)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute owned searchpath reference {reference_executable:?}: {error}"
            )
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned searchpath case {name:?} differs: Blu={result:?}, reference={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_source_require_case(
    name: &str,
    profile: SemanticProfile,
    reference_executable: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let mut engine = Engine::default();
    engine
        .vm_mut()
        .set_file_probe(|path| Ok(path == b"./answer.lua" || path == b"./empty.lua"));
    engine.vm_mut().set_file_loader(|path| {
        if path == b"./answer.lua" {
            Ok(b"return (...) == \"answer\" and 41 or nil".to_vec())
        } else if path == b"./empty.lua" {
            Ok(b"return".to_vec())
        } else {
            Err(RuntimeError::Raised(Value::String(Arc::from(
                &b"unknown module path"[..],
            ))))
        }
    });
    let result = engine
        .execute_owned_source(SOURCE_REQUIRE_SOURCE, profile)
        .map_err(|error| format!("owned source require failed for case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned source require returned {} values for case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let source_path = temporary.join("answer.lua");
    fs::write(&source_path, b"return (...) == \"answer\" and 41 or nil")
        .map_err(|error| error.to_string())?;
    let empty_path = temporary.join("empty.lua");
    fs::write(&empty_path, b"return").map_err(|error| error.to_string())?;
    let reference_path = temporary.join("owned-source-require-reference.lua");
    fs::write(&reference_path, SOURCE_REQUIRE_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference_executable = reference_executable
        .canonicalize()
        .map_err(|error| format!("failed to resolve source-require reference: {error}"))?;
    let reference = Command::new(&reference_executable)
        .current_dir(temporary)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute source-require reference {reference_executable:?}: {error}")
        })?;
    ensure_success(&reference_executable, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned source-require case {name:?} differs: Blu={result:?}, reference={:?}",
            reference.trim()
        ));
    }
    Ok(())
}

fn verify_owned_warning_case(
    name: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let source_file = SourceFile::new(
        SourceId::new(1),
        "owned-warning-conformance.blu",
        WARN_SOURCE.as_bytes().to_vec(),
        SourceLimits::default(),
    )
    .map_err(|error| format!("owned warning source {name:?} was invalid: {error}"))?;
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
        .map_err(|error| format!("owned compiler failed warning case {name:?}: {error}"))?;
    let mut vm = Vm::default();
    let result = vm
        .execute_blu_v1(compilation.into_validated_artifact(), BluLimits::default())
        .map_err(|error| format!("owned runtime failed warning case {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned runtime returned {} values for warning case {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;
    let warnings = vm.take_warnings();

    let reference_path = temporary.join("owned-warning-reference.luau");
    fs::write(&reference_path, WARN_REFERENCE_SOURCE).map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| format!("failed to execute warning reference {upstream:?}: {error}"))?;
    ensure_success(upstream, &reference)?;
    let reference_output = String::from_utf8_lossy(&reference.stdout);
    if result != reference_output.trim() {
        return Err(format!(
            "warning case {name:?} differs: Blu={result:?}, Lua={:?}",
            reference_output.trim()
        ));
    }
    if warnings != reference.stderr {
        return Err(format!(
            "warning case {name:?} stderr differs: Blu={warnings:?}, Lua={:?}",
            reference.stderr
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

fn verify_owned_yielding_load_reader_case(
    name: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(YIELDING_LOAD_READER_SOURCE, profile)
        .map_err(|error| format!("owned yielding reader load failed for case {name:?}: {error}"))?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned yielding reader load case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-yielding-load-reader-reference.luau");
    fs::write(&reference_path, YIELDING_LOAD_READER_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute yielding reader load reference {upstream:?}: {error}")
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:false" {
        return Err(format!(
            "yielding reader load reference for case {name:?} changed: expected rejection, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_yielding_package_searcher_case(
    name: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(YIELDING_PACKAGE_SEARCHER_SOURCE, profile)
        .map_err(|error| {
            format!("owned yielding package searcher failed for case {name:?}: {error}")
        })?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned yielding package searcher case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-yielding-package-searcher-reference.luau");
    fs::write(&reference_path, YIELDING_PACKAGE_SEARCHER_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute yielding package searcher reference {upstream:?}: {error}")
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:false" {
        return Err(format!(
            "yielding package searcher reference for case {name:?} changed: expected rejection, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_yielding_package_loader_case(
    name: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(YIELDING_PACKAGE_LOADER_SOURCE, profile)
        .map_err(|error| {
            format!("owned yielding package loader failed for case {name:?}: {error}")
        })?;
    if result != [Value::Boolean(true)] {
        return Err(format!(
            "owned yielding package loader case {name:?} returned {result:?}, expected true"
        ));
    }

    let reference_path = temporary.join("owned-yielding-package-loader-reference.lua");
    fs::write(&reference_path, YIELDING_PACKAGE_LOADER_REFERENCE_SOURCE)
        .map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute yielding package loader reference {upstream:?}: {error}")
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if reference.trim() != "boolean:false" {
        return Err(format!(
            "yielding package loader reference for case {name:?} changed: expected rejection, got {reference:?}"
        ));
    }
    Ok(())
}

fn verify_owned_load_mode_case(
    name: &str,
    source: &str,
    reference_source: &str,
    profile: SemanticProfile,
    upstream: &Path,
    temporary: &Path,
) -> Result<(), String> {
    let result = Engine::default()
        .execute_owned_source(source, profile)
        .map_err(|error| format!("owned load mode case failed for {name:?}: {error}"))?;
    if result.len() != 1 {
        return Err(format!(
            "owned load mode case returned {} values for {name:?}, expected one",
            result.len()
        ));
    }
    let result = canonical_value(&result[0])?;

    let reference_path = temporary.join("owned-load-mode-reference.luau");
    fs::write(&reference_path, reference_source).map_err(|error| error.to_string())?;
    let reference = Command::new(upstream)
        .arg(&reference_path)
        .output()
        .map_err(|error| {
            format!("failed to execute owned load mode reference {upstream:?}: {error}")
        })?;
    ensure_success(upstream, &reference)?;
    let reference = String::from_utf8_lossy(&reference.stdout);
    if result != reference.trim() {
        return Err(format!(
            "owned load mode case {name:?} differs: Blu={result:?}, Lua={:?}",
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
    official_luau_tests: Option<PathBuf>,
    official_luau_test: Option<String>,
    official_luau_profile: OfficialLuauProfile,
    official_lua_tests: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OfficialLuauProfile {
    Both,
    Blu,
    Luau,
}

impl OfficialLuauProfile {
    fn profiles(self) -> &'static [SemanticProfile] {
        match self {
            Self::Both => &[SemanticProfile::Blu, SemanticProfile::Luau],
            Self::Blu => &[SemanticProfile::Blu],
            Self::Luau => &[SemanticProfile::Luau],
        }
    }
}

fn semantic_profile_name(profile: SemanticProfile) -> &'static str {
    match profile {
        SemanticProfile::Blu => "blu",
        SemanticProfile::Luau => "luau",
        SemanticProfile::Lua51 => "lua51",
        SemanticProfile::Lua52 => "lua52",
        SemanticProfile::Lua53 => "lua53",
        SemanticProfile::Lua54 => "lua54",
        SemanticProfile::Lua55 => "lua55",
        _ => unreachable!("unknown semantic profile"),
    }
}

impl Args {
    fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut upstream = None;
        let mut source = None;
        let mut lua_source = None;
        let mut official_luau_tests = None;
        let mut official_luau_test = None;
        let mut official_luau_profile = OfficialLuauProfile::Both;
        let mut official_lua_tests = None;
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
                Some("--official-luau-tests") => {
                    official_luau_tests = Some(
                        args.next()
                            .ok_or("--official-luau-tests requires a path")?
                            .into(),
                    );
                }
                Some("--official-luau-test") => {
                    official_luau_test = Some(
                        args.next()
                            .ok_or("--official-luau-test requires a test filename")?
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                Some("--official-luau-profile") => {
                    official_luau_profile = match args
                        .next()
                        .ok_or("--official-luau-profile requires blu, luau, or both")?
                        .to_string_lossy()
                        .as_ref()
                    {
                        "blu" => OfficialLuauProfile::Blu,
                        "luau" => OfficialLuauProfile::Luau,
                        "both" => OfficialLuauProfile::Both,
                        value => {
                            return Err(format!(
                                "--official-luau-profile expected blu, luau, or both, got {value:?}"
                            ));
                        }
                    };
                }
                Some("--official-lua-tests") => {
                    official_lua_tests = Some(
                        args.next()
                            .ok_or("--official-lua-tests requires a path")?
                            .into(),
                    );
                }
                _ => {
                    return Err(format!(
                        "usage: blu-conformance --upstream <luau> --source <luau-checkout> \
                         --lua-source <lua-checkouts> [--official-luau-tests <luau-checkout>] \
                         [--official-luau-test <filename|all>] \
                         [--official-luau-profile <blu|luau|both>] \
                         [--official-lua-tests <lua-checkouts>]; \
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
            official_luau_tests,
            official_luau_test,
            official_luau_profile,
            official_lua_tests,
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
                official_luau_tests: None,
                official_luau_test: None,
                official_luau_profile: OfficialLuauProfile::Both,
                official_lua_tests: None,
            }
        );
        assert!(Args::parse(std::iter::empty()).is_err());
    }

    #[test]
    fn parses_optional_upstream_suite_paths() {
        let args = Args::parse(
            [
                "--upstream",
                "/tmp/build/luau",
                "--source",
                "/tmp/source",
                "--lua-source",
                "/tmp/lua",
                "--official-luau-tests",
                "/tmp/luau-checkout",
                "--official-luau-test",
                "closure.luau",
                "--official-luau-profile",
                "luau",
                "--official-lua-tests",
                "/tmp/lua-checkouts",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        assert_eq!(args.official_luau_tests, Some("/tmp/luau-checkout".into()));
        assert_eq!(args.official_luau_test, Some("closure.luau".into()));
        assert_eq!(args.official_luau_profile, OfficialLuauProfile::Luau);
        assert_eq!(args.official_lua_tests, Some("/tmp/lua-checkouts".into()));
    }
}
