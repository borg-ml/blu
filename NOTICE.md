# Attribution

Blu ports and adapts parts of the
[Luau](https://github.com/luau-lang/luau) language runtime. Luau is derived
from Lua 5.1 and is licensed under the MIT License. Blu intentionally extends
the supported language and host surface and must not be represented as an
official or perfectly substitutable Luau implementation.

Compatibility work is pinned to upstream commit
`f8ca77acdcb50241e3da21af663f8ef97b4b5ce4`. Files ported from Luau retain
source-level attribution, and applicable upstream tests are preserved with
their original notices.

The in-process source compiler adapter is built from the MIT-licensed
`luau0-src` crate version `0.20.7+luau728`. Compiler output is validated and
differentially executed against the separately pinned Luau conformance oracle
above; the compiler release is not used to redefine that compatibility target.

Blu is an independent Borg project. It is not sponsored or endorsed by Roblox.

Blu also targets source and C API compatibility with separately versioned
releases of Lua. Lua is copyright Lua.org, PUC-Rio and is MIT licensed. Exact
reference releases and hashes are recorded in `UPSTREAM.toml`.
