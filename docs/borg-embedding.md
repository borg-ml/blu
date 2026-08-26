# Blu in Borg Agent

Borg Agent embeds Blu as its Lua-family workflow runtime. Extension entrypoints
ending in `.blu`, `.lua`, or `.luau` run through Blu; `.luau` selects Luau
semantics while `.blu` and `.lua` use the Borg-selected Blu profile.

Blu owns language execution, values, resource limits, and host-call boundaries.
Borg owns package discovery, configuration, durable workflow receipts, replay,
tool and command registration, and the authority granted to each package. A Blu
script cannot grant itself a Borg capability merely by calling a global name.

## Borg package access modes

Borg packages declare one of three access modes in `blu.toml`:

```toml
runtime_access = "sandboxed" # sandboxed | trusted | native
```

- `sandboxed` uses embedded Blu/Lua/Luau workflows and declared skills. Borg
  does not admit MCP or external workflow processes for the package.
- `trusted` lets Borg supervise MCP and external workflow processes with the
  user's operating-system authority.
- `native` is a Borg loader mode for hash-pinned C ABI libraries. It does not
  change Blu language semantics; the native library runs inside Borg itself.

The user's Borg configuration caps requested authority. Project configuration
cannot raise its own cap. These are Borg host-policy concepts, not dialects:
selecting `--!dialect luau`, for example, never implies sandboxed or trusted
host access.

## Host capabilities

Blu deliberately represents filesystem, process, network, module loading, IO,
and other system effects as host capabilities. The embedding decides which
callbacks exist. Missing capability calls fail through typed host-policy paths;
the VM does not silently acquire ambient authority.

This separation lets the same language artifact execute under different Borg
policies without maintaining separate Lua runtimes. It also means Borg's
package manifest and user policy are the authoritative documentation for what a
workflow may do in a particular session.

## Customizing Borg

Borg's stable extension API exposes workflows, lifecycle hooks, commands,
tools, package settings, durable storage, persistent programming runtimes, and
validated editor customization. A package can contribute a partial editor tree
through `[api.editor]`, keymaps through `[api.keybindings]`, and aliases through
`[api.aliases]`; Borg merges active packages in dependency-first catalog order.
This reaches layout, rendering behavior, themes, alerts, and input without
giving sandboxed Blu code ambient process authority.

Native mode remains the unrestricted escape hatch. Borg's C ABI v2 supplies
resolved configuration, logging, bounded structured event emission, an opaque
instance handle, and optional shutdown; its published header is
`include/borg_extension.h` in Borg Agent releases.

That header is only for compiled native packages. Blu/Lua/Luau workflows do not
consume it. The C ABI is language-neutral and can be exported by Rust, C++, Zig,
or any other compatible compiled language.

Effective state is inspectable with `borg customize inspect --json`, and the
entire user/current-project configuration can be moved with
`borg customize export` and `borg customize import`.

For the version-matched Borg contracts, see the Borg Agent repository:

- `docs/customization.md`
- `docs/blu-extensions.md`
- `configs/extension.example.toml`

For Blu language semantics and capability behavior, continue with
[`language-contract.md`](language-contract.md) and
[`dialect-matrix.md`](dialect-matrix.md).
