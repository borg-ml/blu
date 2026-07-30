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
and runtime semantics exist. The separate `blu-syntax` crate now implements a
bounded byte lexer and small parser/AST slice for the first owned-frontend
program. It includes byte-zero dialect directives, stable raw-byte spans,
retained trivia, the documented `//` profile gate, `local name = expression`,
bare or expression-list `return`, nil/boolean/identifier expressions, shared
decimal integers plus the digit-bearing fraction/exponent subset (`1.5`, `.25`,
`1.`, `1.e2`, `2e3`, and `4.5e-2`), hexadecimal integers with explicit
Lua 5.3–5.5 wrapping-integer versus number-profile lowering, and internal
numeric separators plus binary integers in the Blu and Luau profiles,
hexadecimal exponent forms for Blu and Lua 5.1–5.5, and fractional hexadecimal
forms for Blu and Lua 5.2–5.5,
single- and double-quoted byte strings with common escaped delimiters, backslash,
control escapes, shared decimal byte escapes, `\xXX`, and whitespace-eating `\z` in Blu, Luau, and Lua 5.2–5.5, and
`+`/`-`/`*`/`/`/`%`/`^`/`//`
precedence plus grouping parentheses and unary `not`/`-`/`#` for byte strings.
Unicode escapes use the explicit Luau/Lua 5.3 maximum and Lua 5.4/5.5 extended
UTF-8 maximum, with Blu selecting the extended byte-string range. Parsing retains the explicit profile and rejects
diagnostics without exposing a partial AST. Resolution, lowering, emission,
and execution are separate explicit stages; the public engine never silently
selects this frontend as a fallback. Parser-owned arenas, lists, and diagnostic counts are bounded
and reserve fallibly. `blu-core::DiagnosticLimits` separately bounds each
diagnostic's label text, secondary labels, expected items, raw found bytes,
notes, and help; the lexer and parser construct these values through fallible
APIs and surface `DiagnosticError` through `LexError` or `ParseError`. These
are frontend object limits with structured allocation failures, not a claim
that every process or host allocation is VM-accounted.

The separate `blu_compiler::owned::OwnedCompiler`, also re-exported through
`blu_lang::frontend`, compiles exactly this AST slice: declaration-ordered
locals (with explicit shadowing), decimal integer
literals, truthiness-based boolean `not`, numeric `+`/`-`/`*`, profile-gated `//`, and
an optional final bare or expression-list `return`. Falling off the chunk emits
an EOF-spanned zero-result return. Fixed scalar local name/value lists evaluate
all right-hand expressions before introducing any listed binding, discard
extra values, and initialize missing values to `nil`. Function-call/MULTRET
adjustment remains outside this owned slice. Fixed scalar assignment lists
likewise snapshot every right-hand expression before moving adjusted values
into targets from left to right, permitting swaps without partial-write
observations. An
unresolved assignment target fails in resolution rather than implicitly
selecting global semantics. Semicolons are retained tokens and act as optional
statement separators or empty statements, including after `return`. Lua 5.3--5.5 artifacts store literals through `i64::MAX` as exact
BluV1 Integer constants and use normal IEEE-754 parsing above that; Lua 5.1,
Lua 5.2, and Luau always use the latter Number policy. Blu currently uses the
Number policy for its bootstrap path, which is not a final Blu
numeric-semantics commitment. BluV1 assigns floor division its own feature bit
and opcode, legal only for `luau` and Lua 5.3--5.5; the owned compiler lowers
it for those profiles. Lua 5.1/5.2 reject it during lexing, while Blu rejects
it during lowering until its numeric and metamethod semantics are assigned. It does
not replace the legacy `Compiler`, call the native Luau compiler, or fall back
to it after rejection.
Resolution and lowering rejections use bounded, fallibly constructed
`blu-core::Diagnostic` values with stable codes, phases, profiles, and byte
spans; diagnostic-construction failures remain separate structured errors.
`blu-compiler` disables its `legacy-luau` feature by default, so direct owned
compiler builds do not compile or link the native oracle. The current facade
and conformance runner enable that compatibility feature explicitly until
their source paths migrate.

The BluV1 baseline artifact can be translated only for an explicitly matching
`blu` or `luau` profile. Translation revalidates the artifact under caller
supplied execution limits, rejects nested/upvalue structure and Luau field
widths it cannot preserve, and returns a profile-tagged chunk. It explicitly
rejects BluV1 floor division rather than substituting a Luau opcode.
`Vm::execute_translated` consumes that chunk and derives the root frame's
semantic profile from the retained artifact tag; the configured VM dialect is
only the fallback for ordinary unprofiled chunks. Frames retain that profile
through suspension, and closures retain the creating profile for later calls.
Only `blu` and `luau` translated artifacts are executable in this bootstrap
path. Mixed-profile BluV1 calls remain explicitly unsupported because the
translator rejects nested prototypes and upvalues rather than collapsing a
child profile into its parent. This is a bootstrap path for the baseline
instructions, not the owned resolver, lowerer, or full Blu backend.

`Vm::execute_blu_v1`, exposed for owned compilations as
`Engine::execute_owned_compilation`, is the direct profile-aware path. It
consumes and revalidates the artifact under caller-supplied limits and executes
the single-prototype scalar baseline slice for all seven profiles without
translation. Baseline register moves preserve non-contiguous values without
coercing their types. Escape-free quoted strings are copied as exact source
bytes between their delimiters. Long-bracket strings with any equality depth
are also byte literals and do not process escapes. All profiles discard an
immediate opening LF or CRLF and normalize CRLF to LF. Luau preserves other
lone CR bytes; Lua 5.1–5.5 normalize them to LF, and Blu explicitly chooses the
Lua normalization rule. Per-constant and aggregate payload limits are checked
before allocation. A bare return uses a zero-width validated register
range and produces no values.
The canonical concatenation instruction names independent left and right
registers and executes string/number coercion directly for every profile.
The Luau bootstrap translator rejects it explicitly because Luau's range-form
opcode requires verified contiguous operands; it does not silently rewrite
canonical register semantics.
Canonical comparison instructions likewise name independent operands and
produce Boolean values. `Equal`, `LessThan`, and `LessEqual` are the artifact
primitives; the compiler derives `~=`, `>`, and `>=` with Boolean negation or
operand reversal while preserving source evaluation order. Equality between
unlike scalar types is false. Ordering accepts compatible numeric operands or
two byte strings and otherwise fails structurally at runtime. These rules are
shared by the current scalar owned slice across all profiles. The bootstrap
translator rejects canonical comparisons explicitly rather than substituting
Luau conditional-skip instructions whose control-flow shape is different.
Canonical `JumpIfTruthy` and `JumpIfFalsy` instructions use absolute
instruction targets and are restricted to forward targets in BluV1. Artifact
validation merges definite register initialization from the taken and
fallthrough paths, so a branch cannot make a skipped write appear initialized.
The owned compiler uses these branches for operand-returning, short-circuit
`and` and `or` in every profile. The bootstrap translator rejects them rather
than mapping unproved control-flow structure to Luau jumps. Backward branches
remain unsupported until bounded loop CFG validation is implemented.
An unconditional forward `Jump` completes the structured substrate used by
owned `if`/`elseif`/`else` statements. Nested blocks own their statement lists,
branch locals leave resolver scope at the block boundary, and local debug
ranges end at that boundary. A conditional whose every branch returns does
not acquire an artificial fallthrough return.
Profile-neutral `do`/`end` statements create the same lexical binding and
debug-range boundary without a runtime branch. If their body terminates,
lowering omits unreachable statements that follow in the enclosing block.
BluV1 separately feature-gates backward `Jump` targets. Validation records the
definitely initialized registers at each declared back-edge target and rejects
an edge that cannot preserve that entry state. The owned compiler uses this
substrate for block-scoped `while` loops. Every iteration, including an empty
body, consumes the normal VM instruction budget; backward conditional branches
and unstructured source jumps remain unsupported.
`break` is shared by all profiles and is rejected during parsing outside a
loop. Lowering maintains a nested loop-control stack, so breaks inside nested
conditionals target the innermost active loop exit without escaping outer
loops or lexical scopes.
`continue` is assigned to Blu and Luau only. It is rejected lexically for Lua
5.1–5.5 rather than being treated as an identifier or silently accepted.
Within a loop it terminates the current block path and transfers to the
innermost loop condition.
`repeat`/`until` is shared by every profile. Its body executes before its
condition, and body-local bindings remain in scope through that condition.
For Blu and Luau, `continue` inside `repeat` transfers to the trailing
condition; it does not skip the condition by restarting the body.
The shared numeric-for slice accepts
`for name = initial, limit [, step] do ... end`. Controls are evaluated exactly
once and copied into hidden registers before the loop variable enters scope.
The implicit step is positive one in the profile's number representation.
Explicit steps must currently be provably nonzero numeric literals; positive
and negative directions are lowered explicitly. Dynamic steps fail with
`BLU-COMPILE-0003`, and literal zero fails with `BLU-COMPILE-0004`.
This restriction preserves an upstream conflict: pinned Lua 5.1–5.3 classify
zero with non-negative steps, Lua 5.4–5.5 raise "`for` step is zero", and
pinned Luau classifies zero with non-positive steps. Blu's zero-step behavior
remains unassigned.
BluV1 global load/store instructions use byte-string constants as names and
require the `GLOBALS` feature bit. Validation rejects non-string name
references and reads from uninitialized registers. Direct execution reads and
writes the VM embedding registry; an absent name produces `nil`. The owned
frontend resolves lexical locals first and otherwise lowers scalar identifier
reads and single-target assignments as globals. Global list assignment and
versioned `_ENV`/`getfenv`/`setfenv` behavior remain unsupported.
BluV1 table construction and indexed access require the `TABLES` feature bit.
The owned grammar accepts bounded constructors with sequential array fields,
identifier-keyed fields, and bracket-keyed fields, plus bracket or dot-name
reads and single-target writes. Array fields receive consecutive keys starting
at one in the selected profile's numeric representation. The owned lowering
evaluates each key before its value and emits field assignments in source
order. This operational order is explicit; Lua 5.3–5.5 document constructor
assignment order as undefined, so it is not presented as a stronger Lua
compatibility guarantee. Direct execution allocates through the generational
heap, roots the complete active register file before allocation or growth,
performs raw value-keyed access, and returns `nil` for absent keys. Indexing a
non-table and invalid table keys return structured runtime errors. Indexed
assignment lists, final-field MULTRET expansion, and metamethod-aware owned
table access remain unsupported rather than silently approximated.
Direct BluV1 execution transiently charges its runtime constant vector,
register file, copied string payloads, and largest possible fixed return buffer
against the VM memory configuration, then releases that charge on both success
and structured failure. This does not yet imply that every legacy Luau frame,
native-owned allocation, or GC work buffer is VM-accounted.
It also executes floor division where the dialect matrix assigns it: Luau
numbers and Lua 5.3--5.5 integers or numbers. Integer constants remain a
lossless storage feature, so the executor rejects them explicitly for profiles
whose integer execution semantics are not assigned. Nested prototypes,
upvalues, and the rest of the language remain explicit unsupported structure,
not an implicit compatibility claim.

Ordinary bytecode calls currently run on an owned, bounded VM frame stack.
Suspended callers and their registers are traced as GC roots. Generational
thread values support `coroutine.create`, `resume`, `yield`, `status`, `wrap`,
`running`, `isyieldable`, and `close`, including nested yields and successful
yields through `pcall`. Errors raised after resumption unwind through saved
explicit frames to the nearest suspended `pcall` or `xpcall`; `xpcall` handlers
may themselves yield without losing outer callers. Luau `running` returns one
value and reports the main thread as yieldable; Blu follows modern Lua by
returning `(thread, is_main)` and making the main thread non-yieldable.

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

The initial shared math library includes `math.fmod`. It requires two numeric
arguments and uses truncating remainder semantics, so the result follows the
dividend's sign and is intentionally distinct from the language `%` operator.

Serialized bytecode and mutable embedding inputs are checked again at the
execution boundary. Loaders, compiler artifacts, and portable packages also
expose an opaque `ValidatedChunk` whose safe API permits immutable inspection
or consuming conversion back to a mutable tooling `Chunk`; converting back
requires validation again before execution. A `NEWTABLE` instruction may
request at most 1,048,576
initial array slots and 1,048,576 initial hash slots. Larger requests fail
validation or return a structured runtime error before allocation. The VM also
has configurable captured-output and live arena-object limits. Crossing the
object threshold first performs tracing collection with active registers,
callers, globals, modules, and threads rooted; retained objects then fail with
a structured limit error. Output growth is preflighted and uses fallible
reservation. Guest-driven arena, table, closure-upvalue, and thread-root
capacity growth also uses checked fallible reservation and returns structured
allocation errors. The runtime now exposes deterministic logical byte
accounting and an optional limit for arena slots plus live table buffers,
closure-upvalue buffers, and thread-root buffers. Collection releases live
object-buffer charges while retaining reusable arena-slot charges. This is
still a partial accounting boundary: strings, chunks, VM stacks, the arena
free list, collection work queues, temporary results, and host-owned values are
not included. Dynamic VM-register growth is nevertheless capped and uses
fallible reservation before changing a frame. Initial frame registers,
constants, varargs, call arguments and results, active/continuation root
vectors, caller-stack growth, and coroutine/protected-call wrappers likewise
reserve fallibly before changing logical state. Resumed protected-error
unwinding uses fallible independent frame/caller snapshots, including
registers, constants, varargs, and open-upvalue indexes. Guest-created
coroutine-state entries and `require` loading/cache bookkeeping reserve their
maps before insertion. Native-function and global registries have configurable
entry limits; `try_register_function` and `try_set_global` reserve collection
growth fallibly and reject over-limit mutations atomically. The older
convenience methods remain panic-on-error compatibility wrappers. Built-in
registry backing storage is reserved fallibly during `Vm::try_new`. Results
returned from a native callback are count-bounded (1,000,000 by default,
configurable with `Vm::with_native_result_limit`) and rejected before caller-frame writes, but
allocations performed inside host callback code remain the embedder's
responsibility. Built-in concatenation and the string transformation,
repetition, character, byte-expansion, and `table.unpack` result buffers use
checked fallible reservation; concatenation also enforces the 64 MiB
string-result limit.
Accounted guest heap growth triggers collection at the configured byte
threshold before reserving more storage. Active and saved frames trace both
their values and every live open-upvalue cell.

Global-name storage, the native registry, formatted error strings, allocator
metadata, and host-created callback values still contain allocations outside
this fallible logical boundary.

Heap handles returned by `Vm::execute*` (and therefore `Engine::execute*`) stay
rooted across later calls. Each returned table, closure, or thread occurrence
uses one entry in a bounded host-value retention set (4096 by default,
configurable with `Vm::with_host_value_limit`). Embedders must call
`Vm::release_value`, `Vm::release_values`, or `Vm::release_all_values` after
they no longer use those returned handles. A result that would exceed the
configured retention limit fails atomically with `RuntimeError::HostValueLimit`;
it does not retain only a prefix or alter existing entries. Embedders can
observe the current occurrence count and configured bound with
`Vm::retained_value_count` and `Vm::host_value_limit`. Cloning a `Value` does
not add a retention entry, while returning the same handle again does. Release
exactly once per returned occurrence, after all host clones of that occurrence
are unused.

Automatic execute-result retention is a compatibility convenience, not a
complete ownership-inference system. Values cloned from `Vm::global` or read
through the low-level `Vm::heap` accessor are not execute results. The host
must call `Vm::retain_value` or the atomic batch `Vm::retain_values` before
removing their existing VM root or allowing later VM work to collect. Each
successful explicit retain creates one occurrence that likewise requires one
release. Standalone `Heap` users instead own the root contract directly and
must include every live returned handle in each call to `Heap::collect`. This
accounting remains logical rather than a hard process-wide memory limit.

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
