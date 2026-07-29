# Blu: A language extending Lua with a fast Rust runtime

Blu is a fast, embeddable Lua/Luau superset language and runtime written in
Rust. It is built for deeply extensible native applications. Its default `blu`
dialect pragmatically unifies and extends Luau and modern Lua; explicit
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

The current Luau-backed Blu path compiles source in-process and covers scalar
and table operations, numeric and generic loops, closures and mutable upvalues,
variadic and multiple-return calls, globals and imports, Rust native functions,
protected calls, `pairs`/`ipairs`/`next`, string method dispatch, core
metamethods, an initial standard library, and host-configured cached `require`.
Ordinary bytecode calls use a bounded explicit VM frame stack; saved callers
remain GC roots. Initial generational coroutine threads implement
`create`/`resume`/`yield`/`status`/`wrap`/`running`/`isyieldable`/`close`,
including nested calls, resume arguments, successful protected-call
suspension, resumed `pcall`/`xpcall` error unwinding, yielding error handlers,
and GC-traced continuations.
Portable V1 package envelopes provide bounded canonical decoding, SHA-256
identity, explicit dialect and authority requirements, and an opaque validated
bytecode payload. The public engine currently executes only dialect-matched
pure packages without imports; capability matching and linking fail
explicitly until their host policies exist.
The default engine selects `blu`; `--!dialect` directives are checked against
the configured engine. Lua 5.1–5.5 profiles are declared but still fail
explicitly as unimplemented. This is meaningful execution coverage, not yet a
claim of complete Luau, Lua, or Blu compatibility.

The first Blu-owned frontend substrate is also present: `blu-syntax` performs
bounded byte-oriented lexing and parses the initial local/assignment-list/return arithmetic
slice, including nil, boolean, shared decimal integers plus digit-bearing
fraction/exponent forms (`1.5`, `.25`, `2e3`, `4.5e-2`),
and quoted or long-bracket byte-string literals. Quoted strings implement the
shared escapes plus explicit profile rules for byte, Unicode, whitespace, and
line-continuation escapes; long strings use profile-specific newline handling
plus semicolon separators, grouping parentheses, and profile-neutral
`+`/`-`/`*`/`/`/`%`/`^` plus right-associative `..`, into a spanned arena AST with explicit profile
reconciliation. Unary `not`
follows common Lua truthiness and produces a boolean under every profile;
unary `-` preserves integers in Lua 5.3–5.5 and negates numbers in the other
profiles. Unary `#` measures byte strings, returning an integer for Lua
5.3–5.5 and a number elsewhere; table length remains outside this frontend
slice. Exponentiation follows the shared right-associative precedence above
unary operators and always produces a number.
Hexadecimal integers are accepted in every profile. Lua 5.3–5.5 use their
wrapping 64-bit integer representation; Blu, Luau, Lua 5.1, and Lua 5.2 use
numbers. Internal numeric separators are accepted only by Blu and Luau.
Binary integers are also available only in Blu and Luau. Hexadecimal floats and
their profile matrix are supported: exponent-only forms work in Blu and Lua
5.1–5.5, fractional forms work in Blu and Lua 5.2–5.5, and Luau rejects both.
Other profile-specific numeral extensions remain explicitly unsupported.
Quoted strings support decimal byte escapes in every profile. Two-digit
hexadecimal byte escapes are available in Blu, Luau, and Lua 5.2–5.5; malformed
or out-of-range byte escapes are rejected structurally.
Those same profiles support `\z`, which removes every following ASCII
whitespace byte, including line breaks. Lua 5.1 rejects it explicitly.
Every profile supports backslash line continuation; LF, CRLF, and CR source
line endings normalize to one LF byte in the resulting string.
Unicode escapes are byte-oriented and explicitly versioned: Blu, Luau, and Lua
5.3–5.5 accept `\u{...}` through `0x10ffff`; Blu and Lua 5.4–5.5 additionally
accept the upstream extended UTF-8 range through `0x7fffffff`. Lua 5.1/5.2
reject the syntax.
Trailing-dot forms such as `1.` and `1.e2` are accepted; as in the pinned
runtimes, `1..2` is malformed and must be spaced before future concatenation.
`blu_compiler::owned::OwnedCompiler` resolves and lowers that slice into
canonical BluV1 artifacts without native linkage or fallback; the same
explicit-profile API is available from the public facade as
`blu_lang::frontend`. `Engine::execute_owned_compilation` directly executes
the single-prototype scalar baseline slice for every declared profile without the
Luau compiler or bytecode translator. It revalidates the consumed artifact
under caller-supplied execution limits. Canonical register moves preserve
arbitrary scalar return lists without numeric coercion. BluV1 floor division
executes with Luau number semantics and Lua 5.3–5.5 integer/number semantics; Blu lowering
still rejects it until Blu numeric and metamethod semantics are assigned.
The owned path also directly executes profile-neutral `==`, `~=`, `<`, `<=`,
`>`, and `>=`; ordered comparisons accept only compatible numbers or byte
strings, while equality between unlike scalar types is false.
Operand-returning `and` and `or` use validated forward branches and preserve
short-circuit evaluation in every profile. Backward canonical branches remain
unsupported until loop control-flow validation is assigned.
The older `Engine::execute` source path continues to use the pinned Luau
compatibility compiler while the owned grammar and executor are expanded.

## Repository layout

- `blu-lang`: public facade crate for embedding Blu.
- `blu-core`: dependency-free semantic profiles, source identities, byte spans, and diagnostics.
- `blu-syntax`: bounded byte lexer and initial parser/AST for the Blu-owned frontend.
- `blu-compiler`: safe-Rust BluV1 compiler slice, with an opt-in legacy Luau compiler adapter.
- `blu-bytecode`: bounded BluV1 artifacts plus versioned Luau decoding and loading.
- `blu-package`: bounded canonical package envelopes and artifact validation.
- `blu-runtime`: values, heap, interpreter, interruption, and Rust host API.
- `blu-conformance`: differential execution against pinned Luau and Lua runtimes.
- `.upstream/luau`: ignored checkout created by `just upstream`.

The Rust core, syntax, bytecode, package, runtime, facade, and conformance
crates forbid unsafe Rust at the crate level. `blu-compiler` builds its owned
compiler without native dependencies by default. Its opt-in `legacy-luau`
feature contains the isolated boundary for the pinned upstream Luau C++
compiler; a `noexcept` shim translates native exceptions and owns
allocation/deallocation across that boundary. The current `blu-lang` facade
and conformance runner enable that compatibility feature explicitly.

## Development

```sh
just upstream
just test
just conformance
```

See [NOTICE.md](NOTICE.md) for upstream attribution and [UPSTREAM.toml](UPSTREAM.toml)
for compatibility revisions. The intended compatibility and authority model is
defined in [docs/language-contract.md](docs/language-contract.md). The explicit
profile backlog is tracked in
[docs/dialect-matrix.md](docs/dialect-matrix.md), and the Blu-owned frontend
decision is recorded in
[ADR 0002](docs/adr/0002-blu-owned-frontend.md).

Rust applications should depend on the `blu-lang` crate. The bare `blu` name on
crates.io belongs to an unrelated project.

```rust
use blu_lang::{Engine, Value};

let values = Engine::default()
    .execute("return 20 + 22")
    .expect("valid Blu source");
assert_eq!(values, vec![Value::Number(42.0)]);
```

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

## License

Blu is free and open-source software, available under the [MIT License](LICENSE).
