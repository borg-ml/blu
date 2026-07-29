#include <cstddef>
#include <cstdlib>
#include <new>

extern "C"
{
typedef void* lua_CompileConstant;
typedef int (*lua_LibraryMemberTypeCallback)(const char* library, const char* member);
typedef void (*lua_LibraryMemberConstantCallback)(
    const char* library,
    const char* member,
    lua_CompileConstant* constant
);

// This declaration matches the C API bundled by the exact-pinned luau0-src
// release. Keeping it native prevents Rust layout from participating in the
// ABI and centralizes upgrade review in this compatibility file.
struct lua_CompileOptions
{
    int optimizationLevel;
    int debugLevel;
    int typeInfoLevel;
    int coverageLevel;
    const char* vectorLib;
    const char* vectorCtor;
    const char* vectorType;
    const char* const* mutableGlobals;
    const char* const* userdataTypes;
    const char* const* librariesWithKnownMembers;
    lua_LibraryMemberTypeCallback libraryMemberTypeCb;
    lua_LibraryMemberConstantCallback libraryMemberConstantCb;
    const char* const* disabledBuiltins;
};

char* luau_compile(const char* source, size_t size, lua_CompileOptions* options, size_t* outsize);
}

namespace
{
enum Status
{
    StatusOk = 0,
    StatusAllocation = 1,
    StatusException = 2,
    StatusInvalidArgument = 3,
};

template<typename Callback>
int guard(Callback&& callback) noexcept
{
    try
    {
        return callback();
    }
    catch (const std::bad_alloc&)
    {
        return StatusAllocation;
    }
    catch (...)
    {
        return StatusException;
    }
}
} // namespace

extern "C" int blu_luau_compile(
    const char* source,
    size_t source_size,
    int optimization_level,
    int debug_level,
    int type_info_level,
    int coverage_level,
    char** output,
    size_t* output_size
) noexcept
{
    if (!output || !output_size || (!source && source_size != 0))
        return StatusInvalidArgument;

    *output = nullptr;
    *output_size = 0;

    return guard([&]() {
        lua_CompileOptions options{};
        options.optimizationLevel = optimization_level;
        options.debugLevel = debug_level;
        options.typeInfoLevel = type_info_level;
        options.coverageLevel = coverage_level;

        const char* input = source_size == 0 ? "" : source;
        char* result = luau_compile(input, source_size, &options, output_size);
        if (!result)
        {
            *output_size = 0;
            return StatusAllocation;
        }

        *output = result;
        return StatusOk;
    });
}

extern "C" void blu_luau_free(void* pointer) noexcept
{
    std::free(pointer);
}

// Rust declares this only under cfg(test). It deterministically exercises the
// exact exception translator used by the production entrypoint.
extern "C" int blu_luau_test_exception_status(int kind) noexcept
{
    return guard([=]() {
        if (kind == 1)
            throw std::bad_alloc();
        if (kind == 2)
            throw kind;
        return StatusOk;
    });
}
