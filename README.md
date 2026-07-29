# Blu

Blu is a high-performance Lua-family language and Rust runtime, built for
deeply extensible native applications and the Borg agent runtime. Its default
`blu` dialect is a pragmatic superset of Luau and modern Lua; explicit
compatibility dialects preserve the exact semantics of each upstream language
version where those semantics conflict.

Blu starts from [Luau](https://github.com/luau-lang/luau)'s optimized language
and VM design, but it is not confined to Roblox's sandboxed Luau surface. Blu
supports explicit Luau and Lua compatibility modes alongside a first-class
systems profile with `io`, `os`, packages, filesystem, network, and process
capabilities.

Blu is currently under active compatibility development. The initial milestone
is a safe Luau-bytecode loader and interpreter continuously checked against a
pinned upstream Luau revision. Lua 5.1–5.5 source profiles, standard libraries,
and versioned C API bridges follow behind the same differential conformance
gates.

## Repository layout

- `blu-lang`: public facade crate for embedding Blu.
- `blu-bytecode`: versioned Luau instruction decoding and serialized chunk loading.
- `blu-runtime`: values, heap, interpreter, interruption, and Rust host API.
- `blu-conformance`: differential execution against pinned Luau and Lua runtimes.
- `.upstream/luau`: ignored checkout created by `just upstream`.

## Development

```sh
just upstream
just test
just conformance
```

See [NOTICE.md](NOTICE.md) for upstream attribution and [UPSTREAM.toml](UPSTREAM.toml)
for compatibility revisions. The intended compatibility and authority model is
defined in [docs/language-contract.md](docs/language-contract.md).

Rust applications should depend on the `blu-lang` crate. The bare `blu` name on
crates.io belongs to an unrelated project.

## Intended embedders

The core runtime is application-neutral and does not depend on Borg. It is
intended for:

- extensible terminal and desktop applications;
- game engines and simulation tools;
- command-line automation;
- servers and edge runtimes;
- build, workflow, and configuration systems;
- editors and developer tools;
- agent and orchestration platforms.

Application adapters such as a future `blu-borg` crate belong outside the core
runtime and use the same embedding API available to third-party applications.
