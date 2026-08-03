global <const> *
local f, error_message = load([[local a = {4
]])
print(error_message)
local code, code_message = load("for x do")
print(code, code_message)
local method_code, method_message = load("x:call")
print(method_code, method_message)
local ok_utf8, utf8_message = pcall(function()
    for _ in utf8.codes("ab\xff") do end
end)
print(ok_utf8, utf8_message)
for _, value in ipairs({"ab\xff", "\u{110000}", "in\x80valid", "\xbfinvalid", "αλφ\xBFα"}) do
    local ok, message = pcall(function()
        for codepoint in utf8.codes(value) do
            assert(codepoint)
        end
    end)
    print("codes", ok, message)
end
local text = "hello World"
print("charpattern", utf8.charpattern)
print("pattern", string.find("h", "^" .. utf8.charpattern .. "$"))
for index = 1, #text do
    local start = utf8.offset(text, index)
    local next_start = utf8.offset(text, 2, start)
    print("offset", index, start, next_start, string.sub(text, start, next_start - 1), string.find(string.sub(text, start, next_start - 1), "^[%z\1-\127\194-\244][\128-\191]*$"))
end
local unicode = "汉字/漢字"
for index = 1, utf8.len(unicode) do
    local start = utf8.offset(unicode, index)
    local next_start = utf8.offset(unicode, 2, start)
    local chunk = string.sub(unicode, start, next_start - 1)
    print("unicode", index, start, next_start, chunk, string.find(chunk, "^" .. utf8.charpattern .. "$"), utf8.codepoint(unicode, start, next_start - 1))
end
local const_code, const_message = load("local x, y <const>, z = 10, 20, 30; x = 11; y = 12")
print("const", const_code, const_message)
local function checkro(name, code)
    local state, message = load(code)
    local expected = string.format("attempt to assign to const variable '%s'", name)
    print("checkro", name, state, message, expected, message and string.find(message, expected))
end
checkro("y", "local x, y <const>, z = 10, 20, 30; x = 11; y = 12")
checkro("x", "local x <const>, y, z <const> = 10, 20, 30; x = 11")
do
    global assert<const>, load, string, X
    X = 1
    local state, message = load("local x, y <const>, z = 10, 20, 30; x = 11; y = 12")
    print("global checkro", state, message, string.find(message, "attempt to assign to const variable 'y'"))
end
local function checkro_all(name, code)
    local state, message = load(code)
    local expected = string.format("attempt to assign to const variable '%s'", name)
    print("all", name, state, message, message and string.find(message, expected))
end
checkro_all("z", "local x <const>, y, z <const> = 10, 20, 30; y = 10; z = 11")
checkro_all("foo", "local<const> foo = 10; function foo() end")
checkro_all("foo", "local<const> foo <const> = {}; function foo() end")
checkro_all("foo", "global<const> foo <const>; function foo() end")
checkro_all("XX", "global XX <const>; XX = 10")
checkro_all("XX", "local _ENV; global XX <const>; XX = 10")
checkro_all("z", [[
    local a, z <const>, b = 10;
    function foo() a = 20; z = 32; end
]])
checkro_all("var1", [[
    local a, var1 <const> = 10;
    function foo() a = 20; z = function () var1 = 12; end  end
]])
checkro_all("var1", [[
    global a, var1 <const>, z;
    local function foo() a = 20; z = function () var1 = 12; end  end
]])
local close_state, close_message = load("local <close> a, b")
print("close", close_state, close_message)
local close_state2, close_message2 = load("local a<close>, b<close>")
print("close2", close_state2, close_message2)
