use blu_runtime::{
    Value, Vm,
    bytecode::{LoadLimits, disassemble, load},
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
    verify_portable_reference("Luau", &args.upstream, &portable_source)?;
    for (name, executable) in &lua_references {
        verify_portable_reference(name, executable, &portable_source)?;
    }

    println!("pinned Luau revision: {PINNED_REVISION}");
    println!("bytecode version: {bytecode_version}");
    println!("scalar differential corpus: pass ({scalar_count} cases)");
    println!(
        "program differential corpus: pass (tables, loops, iteration, methods, metamethods, closures, captures, varargs, multret)"
    );
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
    let result = Vm::default().execute(&chunk).map_err(|error| {
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
        let blu_result = Vm::default()
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
