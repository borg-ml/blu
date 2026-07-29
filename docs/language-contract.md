# Blu language and compatibility contract

Status: early implementation contract

## Purpose

Blu is a fast Lua-family language and Rust runtime for deeply programmable
native applications. The `blu` dialect is a pragmatic superset of Luau and
modern Lua features. Luau provides the initial compiler/VM architecture and one
compatibility target; it does not define the complete Blu language or standard
library.

Exact compatibility remains dialect-specific because Lua and Luau have
conflicting observable semantics that no single mode can preserve
simultaneously.

The runtime must support ordinary system programming where authority permits:
files, streams, environment variables, processes, sockets, dynamic packages,
and native modules are product requirements rather than permanently omitted
sandbox features.

## Dialects

Every source module and serialized function prototype has an explicit dialect.
The compiler never guesses semantics from syntax after a module has been
published.

| Dialect | Contract |
|---|---|
| `luau` | Pinned upstream Luau source and runtime behavior |
| `lua51` | Lua 5.1 source, library, and language semantics |
| `lua52` | Lua 5.2 source, library, and language semantics |
| `lua53` | Lua 5.3 source, integer, bitwise, and library semantics |
| `lua54` | Lua 5.4 source, close-variable, GC, and library semantics |
| `lua55` | Lua 5.5 source, declaration, vararg-table, and library semantics |
| `blu` | Typed Lua-family syntax with Blu modules and system capabilities |

The dialect is selected by an embedding option, package manifest, CLI flag, or
an initial source directive such as `--!dialect lua54`. `auto` may be offered
by import tooling, but imported packages must be locked to the resolved
dialect.

Current implementation status: the public engine defaults to `blu`, accepts
`blu` and `luau` source through the pinned Luau compiler, and rejects a source
directive that conflicts with the configured engine. The Lua 5.1–5.5 profiles
remain explicit, structured `not implemented` errors until their own frontends
and semantic profiles exist.

Ordinary bytecode calls currently run on an owned, bounded VM frame stack.
Suspended callers and their registers are traced as GC roots. Generational
thread values support `coroutine.create`, `resume`, `yield`, `status`, `wrap`,
`running`, `isyieldable`, and `close`, including nested yields and successful
yields through `pcall`. Luau `running` returns one value and reports the main
thread as yieldable; Blu follows modern Lua by returning `(thread, is_main)` and
making the main thread non-yieldable. Protected error unwinding after a resume
remains an explicit compatibility gap.

## Semantic profiles

Parsing different syntaxes into one instruction set is insufficient. Function
prototypes retain the semantic profile needed for behavior that conflicts
between language versions, including:

- number and integer arithmetic;
- floor division and bitwise operators;
- environments (`setfenv` versus `_ENV`);
- proper tail calls;
- equality, ordering, length, pairs, and iteration metamethods;
- coroutine yield boundaries;
- table constructor and assignment ordering;
- finalizers and to-be-closed variables;
- error text where conformance requires it;
- standard-library return conventions.

Calls across dialects convert values but execute the callee under the callee's
profile.

Implemented profile decisions are recorded when the references conflict. For
example, pinned Luau ignores the optional separator passed to `string.rep`,
while the `blu` profile accepts the modern Lua separator form. Blu
`string.lower` and `string.upper` operate deterministically on ASCII bytes and
leave all other bytes unchanged.

## Authority profiles

Language compatibility and host authority are orthogonal.

| Profile | Host access |
|---|---|
| `pure` | Deterministic computation and explicitly supplied host functions |
| `confined` | Capability-granted paths, origins, commands, environment, and clocks |
| `trusted` | Full current-user filesystem, process, network, and dynamic-library authority |

The `io`, `os`, `package`, filesystem, network, and process modules exist in
the runtime. Operations return a clear permission error when the active
authority profile does not grant the requested resource; APIs do not disappear
merely because a module is confined.

Borg project plugins choose `confined` or `trusted` through project trust.
Built-in Borg behavior remains Rust and pays no Blu initialization cost when no
Blu plugin is active.

## Resource limits

Serialized bytecode and mutable embedding inputs are checked again at the
execution boundary. A `NEWTABLE` instruction may request at most 1,048,576
initial array slots and 1,048,576 initial hash slots. Larger requests fail
validation or return a structured runtime error before allocation. This
initial-capacity bound is only one defense; VM-wide byte accounting and
automatic GC thresholds remain required before confined execution can claim a
hard memory limit.

## Package compatibility

Blu targets:

1. standalone Luau packages;
2. pure-source Lua packages for Lua 5.1 through 5.5;
3. LuaRocks packages after resolving their declared Lua/version/platform constraints;
4. native Lua modules through an explicit versioned C API bridge;
5. LuaJIT-compatible source as a later named profile.

Host-specific packages still require their host. A Neovim plugin needs the
Neovim API, an OpenResty package needs the `ngx` API, and a Roblox package needs
Roblox datatypes/services. Blu may provide adapters, but language compatibility
does not fabricate those environments.

The initial embedding surface exposes a host-configured `require` loader with a
per-VM cache, circular-load detection, and GC-rooted module results. Portable
V1 envelopes canonically declare identity, dialect, bytecode versions,
imports, exports, schema digests, and authority requirements; decoding is
bounded, integrity-checked, and validates the contained bytecode without
executing it. The public engine executes only dialect-matched pure packages
with no imports. Capability matching, linking, filesystem resolution,
dependency locks, signatures, LuaRocks resolution, and native module loading
are not yet implemented and fail explicitly where exposed.

## Native modules

Native modules are not sandboxed bytecode. Blu will expose separately versioned
C API compatibility libraries (for example, a Lua 5.4 bridge) whose public
symbols and behavior are tested against modules compiled with the corresponding
official Lua headers.

Loading native code requires `trusted` authority. Applications that need crash
or memory-corruption isolation can run native packages in a supervised worker;
that worker uses the same Lua-facing package contract but is not equivalent to
in-process ABI loading.

LuaJIT FFI and LuaJIT bytecode are distinct compatibility projects and are not
implied by Lua 5.1 C API support.

## Conformance

Compatibility claims require differential evidence:

- official Lua tests for each supported release;
- pinned Luau conformance and VM tests;
- identical source executed by Blu and its declared reference runtime;
- matching values, output, errors, stack behavior, and observable library behavior;
- C API compile/link/run fixtures for each advertised ABI;
- fuzzing of parsers, bytecode loaders, calls, GC barriers, and cross-dialect values.

A dialect remains experimental until its applicable suite and documented
deviations are published.

## Performance

The disabled runtime has zero startup work. Active runtimes are measured for:

- cold initialization and first module execution;
- warm callback latency;
- resident memory per runtime and module;
- allocation rate and GC pause distribution;
- interpreter throughput;
- Rust/VM host-call overhead;
- coroutine and async wake-up overhead;
- system-library and package-loading overhead.

Performance results are always compared with the pinned Luau/Lua references and
include the benchmark source, machine, compiler, and runtime configuration.
