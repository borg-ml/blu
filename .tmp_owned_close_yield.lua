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
