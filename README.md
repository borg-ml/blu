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
The math slice includes shared truncating `math.fmod`; unlike floor-based `%`,
its remainder preserves the dividend's sign, with modern integer preservation
and zero-divisor errors. `math.atan` follows the
legacy one-argument Luau/Lua 5.1–5.2 contract or modern Blu/Lua 5.3–5.5
`atan2(y, x)` contract according to the active profile; `math.asin` and
`math.acos` follow the shared numeric contract. Profile-aware `math.floor` and
`math.ceil` and the integral result of two-result `math.modf` preserve
number-only legacy behavior or return modern exact integers when representable.
`math.abs` preserves modern integer inputs, while `math.log` explicitly
distinguishes Lua 5.1's ignored extra arguments from modern base selection.
`math.min` and `math.max` preserve the selected modern subtype and upstream
NaN ordering.
Modern-profile `math.type`, `math.tointeger`, and `math.ult` provide numeric
subtype introspection, exact integral conversion, and unsigned comparison.
Blu/Luau profiles also expose `math.clamp`, `math.sign`, and `math.round` with
the pinned Luau edge behavior.
Core `tonumber` conversion preserves profile subtypes, hexadecimal integer and
floating strings, and the explicit-base grammar and overflow behavior of each
profile.
Byte-oriented `string.find` supports literal searches, relative starts, empty
needles, nil misses, explicit plain mode, basic anchors, wildcard bytes,
portable byte classes, class negation, and escaped punctuation under a work
limit. The `%g` graph class follows modern profiles while Lua 5.1 preserves its
literal escape semantics. Bracket sets support byte ranges, classes, and
negation. Unimplemented
Lua-pattern syntax fails structurally instead of being treated as literal text.
All four Lua repetition suffixes are bounded: greedy `*`/`+`/`?` and minimal
`-`.
`string.find` and `string.match` return bounded nested substring and position
captures from the same byte-pattern engine, including `%1` through `%9`
backreferences to completed substring captures and bounded `%bxy` nested byte
pairs. Zero-width `%f[set]` byte frontiers share the bracket-set engine.
`string.gsub` adds bounded string, number, and direct-table replacement,
`%0`, `%1`–`%9`, `%%`, empty-match progress, replacement counts,
profile-specific Lua 5.1 escape handling, and explicit rejection of callback
and table-`__index` replacement paths that require resumable calls.
Blu/Luau `string.split` produces bounded byte-string arrays with Luau-compatible
default, empty-separator, consecutive-separator, and empty-field behavior.
Blu/Luau `table.create` and `table.find` provide bounded preallocation/fill and
raw array search with profile-typed result indices.
`table.clear` retains allocation while removing entries, and `table.clone`
performs a bounded shallow copy with unprotected metatable preservation.
`table.freeze`/`table.isfrozen` enforce shallow immutability through every heap
mutation path; clones of frozen tables remain mutable.
The base library exposes GC-safe `collectgarbage("collect")` and accounted-heap
`collectgarbage("count")`; other version-specific commands fail explicitly.
`table.sort` provides bounded default ordering for uniform number and byte-string
sequences; custom comparator callbacks remain explicit pending resumable calls.
Overlap-safe bounded `table.move` is available for Blu, Luau, and Lua 5.3–5.5,
with explicit rejection in Lua 5.1–5.2 profiles.
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
short-circuit evaluation in every profile.
Structured `if`/`elseif`/`else` blocks execute through the same validated
forward control flow, retain branch-local lexical scope, and support
path-terminating returns.
Shared `do`/`end` blocks provide explicit lexical scope without adding a
control-flow branch; unreachable statements after an unconditional nested
return are not emitted.
Block-scoped `while` loops add separately feature-gated backward branches;
validation records the target's definite-initialization state, and runtime
execution remains subject to the VM instruction limit.
Shared `break` statements are structurally restricted to loop bodies and patch
only the innermost loop's exit, including through nested conditional blocks.
`continue` is an explicit Blu/Luau extension that restarts the innermost loop;
Lua 5.1–5.5 profiles reject it during lexing.
Profile-neutral `repeat`/`until` loops execute their body at least once and
retain body locals through the trailing condition. In Blu and Luau, `continue`
in a repeat loop transfers to that trailing condition.
Shared numeric `for` loops snapshot their controls once, use the profile's
numeric representation for the implicit positive unit step, and scope the
index to the loop. Explicit, provably nonzero numeric-literal steps support
both directions. Literal zero follows the pinned Lua 5.1–5.3 and Luau
non-positive classification; Blu remains unassigned and Lua 5.4–5.5 reject
zero explicitly. Dynamic steps execute for Luau and Lua 5.1–5.3 with
single-evaluation snapshots and runtime direction selection; Blu and Lua
5.4–5.5 reject them until their possible zero case is executable.
Generic `for` now executes iterator/state/control triples in Blu, Luau, and
Lua 5.1–5.3, including bounded final-call adjustment, lexical result
variables, and nil-only termination. Lua 5.4–5.5 reject this syntax during
lowering until their fourth to-be-closed control has real `__close` unwinding.
Canonical BluV1 global loads and stores connect the owned frontend to the VM's
embedding registry. Unknown scalar reads produce `nil`, and scalar writes
persist in the VM; identifier assignment lists can mix locals, captures, and
globals while preserving simultaneous assignment. Environment-rebinding APIs
remain explicitly unsupported.
The owned frontend also supports bounded table constructors with sequential
array, identifier-keyed, and bracket-keyed fields, plus bracket and dot-name
reads and single-target writes. These execute directly through the generational
heap, return `nil` for absent keys, and retain active registers as GC roots
during allocation and table growth. Mixed identifier/index/field assignment
lists snapshot every target and right-hand side before committing writes.
Table and method reads follow `__index` table chains or resumably invoke
closure/native handlers; writes likewise follow `__newindex` chains or
handlers. Binary arithmetic dispatches `__add`, `__sub`, `__mul`, `__div`,
`__mod`, `__pow`, and dialect-gated `__idiv` through the same bounded
continuation path; unary negation likewise dispatches `__unm`. Final-field
vararg and call MULTRET expansion are implemented. Concatenation invokes
left-then-right `__concat` handlers through a resumable continuation when
string/number coercion is unavailable. Comparison continuations preserve
profile-specific handler selection and Lua 5.5's removal of reversed-`__lt`
fallback. Operator event values may themselves be bounded callable-table
chains; their final Blu closures remain on the explicit continuation stack.
Unary `#` measures raw table sequences in Lua 5.1. Other profiles resumably
invoke a present table `__len` closure or native handler and otherwise use the
raw sequence length.
Bounded postfix calls evaluate the callee and fixed scalar arguments
left-to-right and dispatch through the VM's existing
closure/native/table-call path. Scalar contexts produce the first result or
`nil`; a final call in a local or identifier assignment list requests the
remaining bounded result count, truncating excess results and padding missing
results with `nil`. Call statements support side-effecting APIs such as
`print`. A final call or method call in a return statement forwards every
result. Sole Blu closure calls replace the current frame; preceding fixed
return values remain in a GC-rooted bounded continuation and are prepended
after the call completes. Callable tables resolve bounded `__call` chains,
prepend every table receiver, and enter Blu closure handlers through those
same continuations. Remaining resumable callbacks stay explicit later work.
Owned variadic functions support scalar and fixed-width
`...` reads with nil padding, dynamic return forwarding, and dynamic final
call arguments, and final table-constructor expansion, including fixed
prefixes and method receivers; active and suspended varargs remain GC roots.
Final calls in table constructors expand every result through a GC-rooted
resumable table-fill continuation for both Blu closures and native functions.
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
