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

Current implementation status: the legacy public engine defaults to `blu`,
accepts `blu` and `luau` source through the pinned Luau compiler, and rejects a
source directive that conflicts with the configured engine. The separate
profile-aware `Engine::execute_owned_source` entry point compiles and directly
executes the bounded owned baseline for all seven profiles, including Lua
5.1–5.5; this does not promote those profiles beyond the covered slice. The
separate `blu-syntax` crate now implements a
bounded byte lexer and small parser/AST slice for the first owned-frontend
program. It includes byte-zero dialect directives, stable raw-byte spans,
retained trivia, the documented `//` profile gate, `local name = expression`,
bare or expression-list `return`, nil/boolean/identifier expressions, shared
decimal integers plus the digit-bearing fraction/exponent subset (`1.5`, `.25`,
`1.`, `1.e2`, `2e3`, and `4.5e-2`), hexadecimal integers with explicit
Blu/Lua 5.3–5.5 wrapping-integer versus number-profile lowering, and internal
numeric separators plus binary integers in the Blu and Luau profiles,
hexadecimal exponent forms for Blu and Lua 5.1–5.5, and fractional hexadecimal
forms for Blu and Lua 5.2–5.5,
single- and double-quoted byte strings with common escaped delimiters, backslash,
control escapes, shared decimal byte escapes, `\xXX`, and whitespace-eating `\z` in Blu, Luau, and Lua 5.2–5.5, plus nested Blu/Luau backtick interpolation with `tostring` conversion for embedded expressions, and
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
an EOF-spanned zero-result return. Local name/value lists evaluate
all right-hand expressions before introducing any listed binding, discard
extra values, and initialize missing values to `nil`. A final call or method
call expands to the statically bounded number of remaining local slots.
Assignment lists
likewise snapshot every right-hand expression before moving adjusted values
into targets from left to right, permitting swaps without partial-write
observations, and apply the same final-call adjustment. A final call or method
call in a return statement forwards every result. A sole call uses a canonical
tail call; preceding fixed return expressions are retained in a GC-rooted
bounded caller continuation and prepended after the final call completes. Call
arguments and table constructors propagate final dynamic result tails through
bounded continuations.
Identifier targets resolve to active locals, enclosing upvalues, or (when an
explicit lexical `_ENV` is active in Lua 5.2–5.5) fields of that environment;
otherwise they use the VM global registry. Blu/Luau-owned chunks also expose a
self-referential `_G` table view backed by that registry; writes through `_G`
are mirrored into the registry. Semicolons are retained tokens and
act as optional statement separators or empty statements. Blu accepts an
empty statement after `return`; Lua 5.1--5.5 accept the optional single
trailing semicolon but reject a second empty statement after `return`, and
Luau rejects empty statements. Lua 5.3--5.5 artifacts store literals through
`i64::MAX` as exact
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

The BluV1/BluV2 baseline artifact can be translated only for an explicitly matching
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
Blu and Luau accept value-selecting `if condition then value elseif ...
else value` expressions. An `else` value is mandatory, only the selected arm
executes, and chained `elseif` arms lower to validated forward branches. Lua
5.1–5.5 reject this Luau-derived expression syntax during parsing; ordinary
statement-form conditionals remain shared.
The canonical concatenation instruction names independent left and right
registers and executes string/number coercion directly for every profile.
When coercion is unavailable, the runtime selects the left then right
`__concat` handler; owned closures resume through the bounded caller
continuation and contribute only their first result.
The Luau bootstrap translator rejects it explicitly because Luau's range-form
opcode requires verified contiguous operands; it does not silently rewrite
canonical register semantics.
Canonical 64-bit integer bitwise instructions cover `&`, `|`, binary and unary
`~`, `<<`, and `>>` for Blu and Lua 5.3–5.5. Shifts are logical over the
64-bit representation, negative displacements reverse direction, and
magnitudes of at least 64 yield zero. Lua 5.3 retains its upstream
numeric-string conversion; Lua 5.4–5.5 reject strings. Blu selects the Lua
5.4+ conversion rule. Luau and Lua 5.1–5.2 reject the syntax during lexing.
The runtime selects left then right `__band`, `__bor`, `__bxor`, `__shl`, and
`__shr` handlers for non-convertible operands and invokes `__bnot` with the
upstream duplicate unary operand. Owned closures resume through the bounded
caller stack and contribute their first result. The Luau bootstrap translator
rejects these canonical 64-bit instructions rather than narrowing them to
32-bit operations.
Canonical comparison instructions likewise name independent operands and
produce Boolean values. `Equal`, `LessThan`, and `LessEqual` are the artifact
primitives; the compiler derives `~=`, `>`, and `>=` with Boolean negation or
operand reversal while preserving source evaluation order. Equality between
unlike scalar types is false. Ordering accepts compatible numeric operands or
two byte strings. Mixed integer/number equality and ordering compare exactly
across the full signed 64-bit range without lossy integer-to-`f64` conversion;
NaN remains unequal to itself and unordered. Table handler results resume into Boolean conversion. Luau
and Lua 5.1–5.2 require both operands to expose the same comparison handler;
Lua 5.3–5.5 search left then right. Luau and Lua 5.1–5.4 implement a missing
`__le` as `not (right < left)`, while Lua 5.5 removed that fallback. Blu
explicitly selects modern left/right lookup with the Lua 5.4 fallback.
Ordering without the required handler fails structurally. The bootstrap
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
Blu and Luau source profiles support the compound-assignment statements `+=`,
`-=`, `*=`, `/=`, `//=`, `%=`, `^=`, and `..=`. Lua profiles reject their
tokens lexically. Lowering snapshots an indexed receiver, key, and previous
value before evaluating the right-hand expression, then uses the same
arithmetic, concatenation, and resumable metamethod instructions as the
corresponding binary operator. Blu `//=` uses Blu's modern integer-preserving
floor-division rule, while Luau `//=` uses Luau number semantics. Compound
assignment accepts exactly one target.
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
Labels and `goto` are assigned to Blu and Lua 5.2–5.5. Labels use the Lua
`::name::` spelling and resolve within one owned function. Forward and
backward jumps are validated as ordinary BluV1 branches. A jump that crosses
a local-binding scope is rejected until the compiler has explicit upvalue
closing and to-be-closed unwinding for that edge; unresolved and duplicate
labels are structured compile errors. Luau and Lua 5.1 reject the syntax
during lexing.
`repeat`/`until` is shared by every profile. Its body executes before its
condition, and body-local bindings remain in scope through that condition.
For Blu and Luau, `continue` inside `repeat` transfers to the trailing
condition; it does not skip the condition by restarting the body.
In Lua 5.4/5.5, a body-local `<close>` value is closed after the condition has
been evaluated: a false condition closes it before the next body iteration,
while a true condition or `break` closes it on loop exit.
The shared numeric-for slice accepts
`for name = initial, limit [, step] do ... end`. Controls are evaluated exactly
once and copied into hidden registers before the loop variable enters scope.
The implicit step is positive one in the profile's number representation.
Explicit steps are evaluated once; positive and negative directions are
lowered explicitly, while dynamic direction is selected from the snapshotted
value. Literal and dynamic zero follow the pinned split: Lua 5.1–5.3 and Luau
classify zero with non-positive steps. Blu and Lua 5.4–5.5 validate zero at
runtime and raise a structured runtime value, preserving their upstream
zero-step rule.
Owned source execution applies the active profile's numeric-string coercion to
the initial, limit, and explicit-step controls before the loop starts. A
non-numeric control still raises a structured type error, and the coerced
values remain snapshotted for the loop's lifetime.
The owned generic-for slice accepts
`for name [, name ...] in expression [, expression ...] do ... end` in every
profile. Its expression list is evaluated once and adjusted to the iterator,
state, and control triplet; Lua 5.4/5.5 additionally retain the fourth
to-be-closed control. The final call supplies remaining controls through
bounded fixed MULTRET. Each step calls the iterator with state and control,
binds its fixed results, and terminates only when the first result is `nil`.
Lua 5.4/5.5 close that fourth control when the loop body raises; the pinned
owned slice preserves the string error object passed to `__close`.
In the owned Blu and Luau profiles, a table in the iterator position is
prepared through the Luau-compatible `__iter` metamethod when present; absent
that hook it becomes the ordinary `next, table, nil` triplet. The hook is
called once before the first iteration and must return a callable iterator
triplet. Callable functions, callable tables, and callable userdata retain the
ordinary generic-for controls; genuinely non-iterable values and nil/non-callable
`__iter` results produce the profile's structured call/iteration errors.
Lua-family profiles retain their ordinary callable-iterator rule. Blu/Luau
`__iter` callbacks may yield while the surrounding coroutine is suspended: the
callback frame, table root, and pending generic-for call are retained through
the owned native-operation continuation and resume with the returned iterator
triplet. The pinned direct-table slice also observes iterator mutations that add
a later element and performs the terminal iterator call; broader mutation
ordering and close interactions remain outside the contract.
`break` and profile-available `continue` use structured loop scopes. The
owned compiler also parses Lua 5.4/5.5 `<const>` and `<close>` local
attributes, rejects const writes, and executes `__close` on normal scope exit,
`break`, return, `goto`, and protected errors. Error objects are passed to
handlers, reverse-order cleanup continues after a handler error, and yielding
handlers resume through owned coroutine continuations. Explicit
`coroutine.close` now unwinds pending suspended `<close>` values, returning
`(false, error)` when a close handler fails while leaving the thread dead.
Pinned Lua 5.4/5.5 probes do not invoke those handlers merely because an
unreachable suspended coroutine is collected; the owned boundary preserves
that no-implicit-close result. Other abandoned-thread reclamation and full
finalizer/GC semantics remain outside the contract.
BluV1 global load/store instructions use byte-string constants as names and
require the `GLOBALS` feature bit. Validation rejects non-string name
references and reads from uninitialized registers. Direct execution reads and
writes the VM embedding registry; an absent name produces `nil`. The owned
frontend resolves lexical locals first and otherwise lowers scalar identifier
reads and assignments as globals. Lua 5.2–5.5 additionally lower an explicit
`local _ENV = table` to lexical environment field reads/writes, capture that
environment through nested closures, and route assignment lists and global
function declarations through it; Lua 5.2–5.5 also use a rooted default chunk
environment synchronized with the embedding registry. The owned Lua 5.2–5.5
source entry point installs an environment-aware `load`: string chunks return
callable closures with a persistent fourth-argument environment (or the
default environment), and the embedding facade exposes the equivalent
`load_owned_source` API. This slice is differentially checked against the
pinned Lua references. Lua 5.1 additionally supports string `loadstring` and
function-targeted `getfenv`/`setfenv`; Blu and Luau now expose the same bounded
function- and current-thread environment slice, alongside Luau's
`loadstring` compatibility extension. Blu and Luau `loadstring` default to the
thread's global environment even when called by a function with a different
environment, and `setfenv(0, table)` changes that default. Lua 5.2–5.5 keep
these legacy names absent. Owned `load` also accepts a reader
function and concatenates its bounded string chunks; an empty string terminates
the reader as in the reference runtimes. Owned readers may yield from a Lua
coroutine and resume through the same bounded native-operation continuation
machinery as other callback libraries; this is an intentional extension because
the pinned Lua reference binaries reject a reader yield across `load`. `load`
also accepts serialized BluV1 (`BLU\0`) artifacts when binary mode is enabled,
validating them under the caller's artifact limits; foreign Lua binary chunks
remain unsupported. Textual and binary mode rejection preserve the supplied mode
string, but the complete foreign-binary mode matrix remains incomplete. Lua 5.1 supports
current-thread `setfenv(0, table)` rebinding and distinguishes it from the
current closure environment. During a live Lua closure call, numeric
`getfenv(1)` and `setfenv(1, table)` now inspect and rebind that closure's
environment, including subsequent global reads in the same call. The Lua 5.1
    main chunk is also a rebindable closure environment, and global writes through
    that environment honor its `__newindex` chain. Arbitrary deeper non-current
    stack levels and native-frame environments remain unsupported because the owned
    continuation does not retain those frame environment handles. The pinned
    boundary probe records the exact deep-frame rejection separately from the
    supported shallow caller and main-chunk cases.
    Owned Lua 5.1 main-chunk `__newindex` handlers also retain their guest
    continuation across `coroutine.yield`; this is an accepted owned extension,
    because the pinned Lua 5.1 runtime rejects that yield across its metamethod
    boundary.
Lua 5.5 named vararg tables and `global`/`global *`
declarations are supported in the owned frontend, including scoped
undeclared-name rejection, declaration initializers, and propagation into nested
closures.
In Lua 5.5 and Blu, a named vararg is backed by a guest table with a mutable
integer `n`; writes to that table are observed by later reads of the named
vararg. The table is bounded by the active dynamic-register limit and malformed
or oversized `n` values raise a structured runtime error. The pinned Lua 5.5
fixture still has one explicit representation boundary: its
`collectgarbage("count")` probe observes a stack-local named-vararg table,
whereas Blu's table is heap-backed and therefore contributes to the reported
heap count. This does not change the guest table semantics and remains isolated
until Blu exposes an upstream-equivalent GC accounting model.
When no explicit registry value shadows it, `_VERSION` is resolved from the
active frame as `Blu`, `Luau`, or `Lua 5.1` through `Lua 5.5`. This avoids
leaking the VM's configured fallback dialect into an explicitly profiled
artifact. Guest and host global writes override the contextual default using
the ordinary shared registry.
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
non-table and invalid table keys return structured runtime errors. Assignment
lists may mix identifier, bracket-index, and dot-field targets. Every
table/key target is evaluated left-to-right and snapshotted before any
right-hand side is evaluated; every right-hand side is then snapshotted before
right-to-left commit phase begins. This preserves simultaneous assignment,
aliased-target behavior, and the pinned `value[1], value = replacement, other`
behavior. Local declarations
likewise allocate a distinct binding register when initialized from an
existing local, so later rebinding does not alias the two names. Missing table
reads follow `__index` table chains or invoke closure/native handlers through
the bounded caller continuation stack; missing writes do the same for
`__newindex`. Existing keys remain raw writes. Handler operands are
snapshotted before invocation, and method lookup uses the same resumable read.
Luau, Blu, Lua 5.1, and Lua 5.2 bound a chain at 100 steps; Lua 5.3–5.5 use
their pinned 2,000-step limit.
Owned binary arithmetic keeps its direct integer/number fast path. When either
operand is nonnumeric, it selects the left then right `__add`, `__sub`,
`__mul`, `__div`, `__mod`, or `__pow` handler and invokes it with the two
snapshotted operands through the bounded caller continuation. `__idiv` follows
the same rule in Blu, Luau, and Lua 5.3–5.5, matching the profiles where `//`
is legal. Blu integer addition, subtraction, and multiplication wrap through
64 bits; integer modulo uses floor semantics. Mixed integer/number arithmetic
promotes to a number, and `/` and exponentiation always produce numbers. Blu
adopts Lua 5.3+ floor division: integer operands preserve an integer result,
while mixed or floating operands produce a floored number.
Integer division by zero is a structured error; floating division follows
IEEE behavior before flooring. Arithmetic operands also accept
whitespace-trimmed decimal and hexadecimal numeric strings. Blu and Lua
5.4–5.5 preserve an exact parsed integer, while Luau and Lua 5.1–5.3 convert
string operands to numbers; invalid numeric strings continue to metamethod
selection and then fail structurally when no handler exists. Unary negation
invokes `__unm` with the operand in both argument positions, matching the
pinned Lua and Luau implementations. Bitwise metamethod events use the same
resumable handler path.
Arithmetic, unary, concatenation, length, and comparison event values may
themselves be callable tables. The runtime resolves their bounded `__call`
chains before invocation, prepends every callable-table receiver, and keeps a
final Blu closure on the explicit operation continuation.
Owned unary `#` measures the raw sequence length of tables without a `__len`
handler, using the same profile-specific integer/number result subtype as
string length. For the covered sparse-table boundary, Luau and Lua 5.1–5.4
retain the allocated legacy array boundary (a sparse three-slot literal is 3,
while an isolated high hash key is 0); Lua 5.5 retains the compact array
border, while Blu's Luau-compatible guest tables keep a high-only assignment
cluster at zero until key 1 exists. Lua 5.1 ignores table `__len` and therefore
remains raw. Blu, Luau, and Lua 5.2–5.5 resumably invoke a present
closure/native handler and store its first result without applying raw-length
numeric conversion; Lua and Blu pass the operand twice to that handler, while
Luau applies the same two-operand call shape but requires the result to be
numeric. Lua 5.2–5.5 and Blu also consult the type metatable for non-string
scalar operands, so debug-installed numeric, boolean, and nil `__index`,
`__newindex`, arithmetic, and `__len` handlers participate in ordinary
execution. Primitive metatable tables are retained as VM roots until replaced.
Constructor-allocated holes and assignment-created holes are distinct: Lua
5.1–5.4 keep an empty-table assignment such as `t[1] = nil; t[2] = 2` out of
the legacy length boundary, Lua 5.5 retains the contiguous reverse-assignment
border when both `t[2]` and `t[3]` are present, while Blu keeps Luau's
high-only assignment boundary at zero, and Luau retains the pinned
`t[1] = nil; t[2] = 2` boundary. These cases are covered by the assignment
length differential fixture. Blu and Luau guest tables preserve small hash
insertion/traversal order for `pairs`/`next`; lower-level host heap tables do
not promise an observable hash order.
`rawlen` is absent in Lua 5.1; supported profiles return string length for
strings, zero for the covered fractional/infinite-only numeric-key tables, and
a protected error for scalar arguments. `table.maxn` is present in Lua 5.1–5.2,
Luau, and Blu; it retains fractional numeric keys and returns positive infinity
for an infinite key.
BluV1 scalar fixed calls require the `FIXED_CALLS` feature bit. The instruction names
one initialized function register and a validated contiguous range of
initialized argument registers, and initializes one destination. Owned postfix
calls evaluate the callee first and each scalar argument left-to-right, then
copy arguments to that contiguous range. Direct execution delegates to the
existing VM call path, preserving native registration, structured errors, call
limits, and active GC roots. Callable tables resolve bounded `__call` chains
before dispatch; every retry prepends its table receiver, and Blu closure
handlers use the same explicit continuations as scalar, fixed-result, vararg,
return, and table-list calls. Cycles fail at the profile's metatable-loop
bound. The expression result is
the first returned value or `nil`; additional values are discarded. Call
statements discard that scalar result. Colon method calls evaluate their
receiver once, perform resumable table lookup before evaluating explicit arguments,
and pass the receiver as the first argument. `FIXED_MULTI_RESULTS` adds a
canonical call instruction with a validated, statically requested contiguous
result range. Direct execution truncates excess results and pads missing
results with `nil`; the owned frontend uses it for final calls in local and
plain-identifier assignment lists. Bootstrap translation rejects this
instruction explicitly because Luau MULTRET translation is not yet canonical.
`RETURN_CALLS` adds terminal canonical instructions for final-call return
statements. Direct execution forwards every native result and replaces the
current Blu closure frame for sole-call Blu callees, so tail-recursive chains
remain bounded by the instruction limit without consuming caller-stack
capacity. When fixed expressions precede the final call, their validated
register range remains in a GC-rooted caller continuation and is prepended to
all returned values. Method return calls preserve the same single receiver
evaluation and implicit first argument as scalar method calls. Bootstrap
translation rejects return calls explicitly.
BluV1 reserves the `CLOSURES`
feature for canonical `NewClosure`, `GetUpvalue`, and `SetUpvalue`
instructions. Validation resolves child indices through the parent's declared
child list, verifies every capture against initialized parent registers or
declared parent upvalues, and bounds every upvalue access. Encoding and
decoding preserve this metadata. Direct execution stores Blu artifacts in the
existing generational closure arena, shares mutable captures through
generational upvalue cells, refreshes suspended parent registers after child
returns, and uses an explicit caller stack bounded by the VM call limit.
Registers, active closures, open upvalues, and suspended callers participate
in allocation roots. Bootstrap translation continues to reject closure
instructions explicitly. Variadic owned functions retain `...` separately
from named parameters and declare the `VARARGS` feature. Fixed scalar reads
and fixed adjustment such as `local first, second = ...` use a validated
destination range, truncate excess arguments, and pad missing arguments with
`nil`. Direct dynamic returns and fixed-prefix forms such as `return head, ...`
forward every vararg through bounded caller continuations. Final call
arguments such as `target(prefix, ...)` and `receiver:method(...)` preserve
their fixed prefix and append the complete vararg vector for fixed-result and
tail-return calls. Final constructor fields such as `{head, ...}` append every
vararg at consecutive one-based array indices with a validated positive start.
A final call field likewise consumes every result through a resumable
`DYNAMIC_CALL_RESULTS` table-fill continuation; suspended frames, outer
callers, the destination table, and not-yet-inserted return values remain GC
roots during growth. Active and saved-frame vararg vectors are GC roots.
Final call arguments such as `target(prefix, producer())` consume every result
from the producer. Canonical adjacent producer/consumer instructions prevent
control flow from entering between the calls; the pending result vector is
bounded by the dynamic stack limit and remains rooted through closure, native,
nested, variadic, fixed-result, and tail-return consumers. Earlier call
arguments still adjust to one result.
General resumable direct-BluV1 callbacks remain unsupported.
The owned parser represents anonymous `function (...) ... end` expressions
and both `local function name(...) ... end` and simple
`function name(...) ... end` declarations with bounded parameter vectors and
function-owned lexical blocks. Loop-control scope is reset at every function
boundary. Simple named declarations install the resulting closure in the VM
global registry. Dotted declarations traverse local, captured, or global table
roots through canonical raw `GetTable` operations and store the closure in the
final field. Colon-method declarations add an explicit compiler-owned `self`
binding before source parameters and store the closure in the named method
field. The binding has a real debug name but no fabricated source span;
existing colon calls supply the receiver exactly once as argument zero.
The owned compiler lowers noncapturing functions to
recursive BluV1 prototype trees, emits `NEWCLOSURE`, and records fixed
parameters for bounded child-frame argument copying. This path executes in all
seven explicit profiles. Lexical resolution emits `GETUPVALUE` and
`SETUPVALUE` for direct, mutable, self-recursive, and transitively nested
captures. Intermediate prototypes explicitly forward ancestor cells with
`ParentUpvalue`; direct parents expose live registers with `ParentRegister`.
Local-function destinations are initialized before closure construction so
recursive capture is structurally valid, then synchronized through the shared
generational upvalue cell when the closure is installed.
Direct BluV1 execution transiently charges its runtime constant vector,
register file, copied string payloads, and largest possible fixed return buffer
against the VM memory configuration, then releases that charge on both success
and structured failure. This does not yet imply that every legacy Luau frame,
native-owned allocation, or GC work buffer is VM-accounted.
It also executes floor division where the dialect matrix assigns it: Luau
numbers and Blu/Lua 5.3--5.5 integers or numbers. Integer constants remain a
lossless storage feature. Blu and Lua 5.3–5.5 materialize them exactly, while
profiles whose integer execution semantics are not assigned reject them.
Nested prototypes,
upvalues, and the rest of the language remain explicit unsupported structure,
not an implicit compatibility claim.

Legacy Luau-bytecode calls currently run on an owned, bounded VM frame stack.
Suspended callers and their registers are traced as GC roots. Generational
thread values support `coroutine.create`, `resume`, `yield`, `status`, `wrap`,
`running`, `isyieldable`, and `close`, including nested yields and successful
yields through `pcall`. Errors raised after resumption unwind through saved
explicit frames to the nearest suspended `pcall` or `xpcall`; `xpcall` handlers
may themselves yield without losing outer callers. Luau `running` returns one
value and reports the main thread as yieldable; Luau reports native callback
frames as non-yieldable while direct guest frames remain yieldable. Blu follows
modern Lua by returning `(thread, is_main)` and making the main thread
non-yieldable. Owned BluV1 coroutine entry closures now have a native
continuation representation:
direct `coroutine.yield` calls resume repeatedly, preserve captured state, and
remain GC-rooted while suspended. Native library operations that invoke
yielding callbacks still require operation-specific continuations and remain
explicit unsupported features.
Owned Luau chunks additionally support the bounded Luau debug surface:
`debug.traceback` accepts the thread/message/level overload and reports
suspended or failed coroutine frames, while `debug.info` supports the portable
`n`, `s`, `l`, `f`, and `a` selectors. The default Blu profile keeps this
Luau-only `debug.info` member hidden under its documented debug-library policy.
Coroutine `resume` and `wrap` preserve the same profile-specific target error
value split as protected calls: legacy Lua 5.1–5.2/Luau stringify numeric
errors, Lua 5.3–5.5/Blu preserve them, and Lua 5.5 renders a nil failure as
`"<no error object>"`. The result and dead-status shape is differentially
covered. Dead-thread resume diagnostics are `"cannot resume dead coroutine"`;
running-thread diagnostics are `"cannot resume running coroutine"` in Lua 5.1
and Luau and `"cannot resume non-suspended coroutine"` in Lua 5.2–5.5 and Blu.
Post-yield table errors and coroutine-close failure objects preserve their
raised values. `coroutine.close` returns success for new/dead threads. Closing a
running coroutine raises in Luau/Blu and Lua 5.4, while Lua 5.5's running
coroutine close returns zero values; Lua 5.5 also reports a main-thread close as
`"cannot close main thread"`. The optional `isyieldable(thread)` argument
returns false for the main thread and true for other threads in Blu and Lua
5.4–5.5; Lua 5.3/Luau retain their current-thread result. `wrap` preserves
table error objects. Invalid thread arguments use the profile's standard
`invalid argument` (Luau/Blu) or `bad argument` (Lua 5.4–5.5) convention, and
dead-thread close preserves the original string error value. These boundaries
are differentially covered.
The protected-call boundary preserves arbitrary raised values in `pcall` and
passes the first error-handler result through `xpcall`. Lua 5.1's `xpcall`
signature ignores arguments after the handler; Lua 5.2–5.5, Blu, and Luau
forward them to the protected function. This profile split is covered by the
pinned argument-forwarding differential case. If an `xpcall` handler itself
raises, all profiles return `(false, "error in error handling")`; a yielding
handler remains a resumable coroutine boundary rather than being converted to
that string. Target error values follow the pinned profile split: Lua 5.1–5.2
and Luau stringify numeric error values, while Blu and Lua 5.3–5.5 preserve
them; Lua 5.5's `pcall` renders a nil target error as `"<no error object>"`,
while its `xpcall` handler still receives nil. The protected-value, handler-
failure, and yielding-handler cases are differentially covered. Legacy source-
location prefixes on stringified numeric diagnostics remain isolated because
the bounded structured error value does not retain an originating source level.
The owned main-thread `pcall`/`xpcall` path now drives non-yielding BluV1 target
closures through an explicit iterative activation scheduler. It preserves
multi-result success values, arbitrary target error values, xpcall handler
results, captured frame environments, GC roots, the shared instruction/deadline
budget, and the configured VM call limit; the focused regressions cover 64-deep
pcall, 1,000-deep xpcall termination, call-limit failure, and collection while
pending frames are live. Coroutine-running calls and protected activations
started from an xpcall handler remain on the existing synchronous/coroutine
boundary, where yielding or a missing resumable activation is reported as a
structured unsupported feature. The upstream Luau 10,000-result protected-stack
case remains isolated because its native 20,000-call stack contract is not the
same as Blu's configured bounded call-limit contract.
Within the owned BluV1 source path, `error(message)` and
`error(message, 1)` prefix string messages with the current source and line,
while level `0` preserves the raw message; profile-specific integer validation
and legacy truncation follow the selected profile. Deeper stack-level source
selection is pinned as a known owned/reference boundary, and source prefixes
for unowned translated frames remain isolated.

The owned standard-library slice now includes profile-gated `utf8.len`,
`utf8.codepoint`, `utf8.char`, `utf8.offset`, `utf8.codes`, and
`utf8.charpattern` for Blu and Lua 5.3–5.5. `utf8.offset` follows the
byte-boundary rules of the selected reference; `utf8.charpattern` is the
binary Lua pattern over UTF-8 lead/continuation-byte ranges; Lua 5.5 additionally returns
the final byte position. `utf8.codes` is a bounded stateful iterator over byte
positions and code points. Blu and Lua 5.4–5.5 also expose `warn` through a
separate bounded warning channel; Lua 5.1–5.3 do not expose that global.
Invalid UTF-8 is reported through the Lua-compatible `utf8.len` result pair;
invalid sequences passed to `utf8.codepoint` remain structured library errors.
Blu, Lua 5.3, and Luau reject `utf8.char` values above `0x10ffff`, while Lua
5.4–5.5 retain their wider `0x7fffffff` encoding range. Lua 5.1–5.2 do not
expose this global. Filesystem, native-module, yielding-loader, and other
system-capability library surfaces remain explicitly incomplete.
Lua 5.4–5.5 also honor the optional lax flag on `utf8.len` and
`utf8.codepoint`, and the optional second argument to `utf8.codes`; in lax
mode their legacy five- and six-byte encodings are decoded through
`0x7fffffff`. Blu and Lua 5.3 retain their always-surrogate-tolerant behavior,
while Luau remains strict.
The optional indices for `utf8.len` and `utf8.codepoint` validate against the
same initial/final bounds as their selected Lua-family reference; the count
and position for `utf8.offset`, and the code points passed to `utf8.char` require
exact integer-representable numbers in Blu and Lua 5.3–5.5; Luau truncates
finite fractions. The profile-gated absence of `utf8` remains unchanged.
The `os.date` timestamp and `debug.traceback` level follow the same exact
integer versus legacy/Luau truncating split. Their host-authorized surfaces
remain hidden in Blu, while Luau retains the pinned standard functions.
The base `tostring` function dispatches a value's `__tostring` metamethod when
present and requires its first result to be a string. Blu and the owned Luau
profile support a yielding stringification callback inside a coroutine through
a rooted native-operation continuation; yields outside a coroutine remain a
structured unsupported boundary. Pinned Lua profiles and Luau reject the same
yield across their native boundary. Default object formatting uses the standard
`table:`, `function:`, `thread:`, `userdata:`, or `lightuserdata:` prefixes;
the identity suffix remains intentionally host-specific. The pinned legacy
Lua 5.1–5.4 scalar rendering path uses the reference's 14-significant-digit
number form for `tostring`, `print`, `io.write`, and traceback messages, while
Blu, Luau, and Lua 5.5 retain shortest round-tripping number rendering. The
`assert` message coercion split remains isolated: Lua 5.1–5.2 and Luau coerce
string-or-number messages, while Blu and Lua 5.3–5.5 preserve arbitrary error
values.

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
leave all other bytes unchanged. Pinned Luau and Lua 5.1–5.2 ignore a second
argument to `math.atan`; Lua 5.3–5.5 interpret it as the `x` coordinate for
`atan2(y, x)`. Blu selects the modern two-coordinate form and defaults `x` to
one when omitted. `math.asin` and `math.acos` use the shared numeric contract
in every profile, return NaN outside their real domains, and reject non-numeric
arguments structurally; numeric strings are coerced through the active profile
parser. `math.floor` and `math.ceil` return numbers for Luau
and Lua 5.1–5.2. Blu follows Lua 5.3–5.5 by returning exact integers when the
rounded value fits `i64`, retaining a floating result for finite out-of-range
values, infinities, and NaN. `math.modf` uses the same profile split for its
truncation-toward-zero integral result and returns the signed fractional part
as a number. `math.modf` accepts numeric strings through the active profile's
number parser before applying that split; the covered `math.tointeger` and
`math.ult` boundaries retain exact integer conversion and reject fractional
inputs.
`math.abs` likewise preserves integer inputs in Blu and Lua 5.3–5.5, including
the upstream wrapping minimum-integer result, and returns numbers elsewhere.
Lua 5.1 ignores extra `math.log` arguments; Blu, Luau, and Lua 5.2–5.5 use the
second argument as the logarithm base.
`math.min` and `math.max` retain the selected operand's integer subtype in Blu
and Lua 5.3–5.5, return numbers in legacy profiles, and use upstream ordered
selection so NaN does not silently replace or get replaced by another operand.
Mixed integer/number selection uses the same exact full-range comparison as
source operators.
`math.mininteger` and `math.maxinteger` expose the exact signed 64-bit bounds in
Blu and Lua 5.3–5.5. They are absent in Luau and Lua 5.1–5.2.
`math.type`, `math.tointeger`, and unsigned integer comparison `math.ult`
follow the Lua 5.3–5.5 contracts in those profiles and Blu. They are absent in
Luau and Lua 5.1–5.2, where the functions do not exist upstream; calling the
missing field follows the ordinary nil-call error.
`math.frexp` returns a binary fraction plus an exponent number in Luau and Lua
5.1–5.2 or an exponent integer in Blu and Lua 5.3–5.5. It preserves signed
zero and handles subnormal and non-finite values without an intermediate
overflow. `math.ldexp` composes the pair; Luau and Lua 5.1–5.2 truncate a
fractional exponent, while modern profiles require an integer-representable
exponent.
The legacy `math.sinh`, `math.cosh`, `math.tanh`, `math.log10`, and
`math.atan2` names exist in Blu, Luau, and Lua 5.1–5.4. Lua 5.5 removed them,
so the names are absent from the Lua 5.5 math table.
`math.random()` returns a number in `[0, 1)`. Bounded results are numbers in
Luau and Lua 5.1–5.2 and integers in Blu and Lua 5.3–5.5. Luau and Lua 5.1
truncate fractional bounds, Lua 5.2 rounds them, and modern profiles require
integer-representable bounds. `math.random(0)` selects the full signed integer
range only in Blu and Lua 5.4–5.5; earlier profiles reject the empty interval.
`math.randomseed(x, y)` returns no values in Luau and Lua 5.1–5.3 and returns
the two effective integer seeds in Blu and Lua 5.4–5.5. Only those modern
profiles permit an omitted seed. Legacy seed arguments truncate fractional
values, while Blu and Lua 5.4–5.5 require integer-representable seeds. Blu's
generator is deterministic and non-cryptographic: equal explicit seeds
produce equal streams, but the exact stream is not promised to match any
upstream implementation or remain a portable language guarantee.
Blu and Luau expose the Luau numeric extensions `math.clamp`, `math.sign`,
`math.round`, `math.isnan`, `math.isinf`, `math.isfinite`, `math.lerp`, and
`math.map`. The arithmetic helpers coerce numeric strings through the active
profile parser, return numbers, preserve Luau NaN behavior, use
ties-away-from-zero rounding, and reject inverted clamp bounds structurally.
`math.lerp` returns its second endpoint exactly when its factor is one,
preserving the pinned overflow-avoidance behavior. Lua profiles leave these
names absent from the corresponding standard libraries. `math.noise` ports the
pinned Luau three-dimensional Perlin implementation,
including its `f32` intermediates, optional zero-valued coordinates, 256-unit
input wrapping, and deterministic outputs. Blu exposes the same contract; Lua
profiles leave the extension absent.
The `bit32` library exposes `band`, `bor`, `bxor`, `bnot`, `lshift`, `rshift`,
`arshift`, `lrotate`, `rrotate`, `extract`, and `replace` in Blu, Luau, Lua
5.2, and Lua 5.3 profiles. Luau truncates numeric inputs toward zero, Lua 5.2
rounds them ties-to-even, and Lua 5.3 requires integer-representable inputs;
strings use the active profile's numeric grammar. Results are numbers in Luau
and Lua 5.2 and integers in Blu and Lua 5.3. Lua 5.1, 5.4, and 5.5 leave
`bit32` absent. Blu deliberately selects Luau's input conversion with Lua 5.3-style
integer results. Field offsets and widths are range-checked structurally.
`tonumber` preserves existing numeric subtypes and integer string conversions
for Blu and Lua 5.3–5.5, returns numbers for legacy profiles, accepts ordinary
hexadecimal integer and floating strings, and follows profile-specific explicit-base parsing,
non-finite spelling acceptance, and overflow behavior. Differential probes pin
the `inf`/`nan` spelling split, overflowing decimal values, unsigned hexadecimal
wraparound, and the signed 64-bit boundary; broader locale and conversion-edge
coverage remains future work.
Integral counts and bytes returned by `rawlen`, `select("#", ...)`,
`string.len`, `string.byte`, and `table.pack.n` likewise use integers in Blu
and Lua 5.3–5.5 and numbers in legacy profiles. `string.len` accepts numeric
arguments through the ordinary numeric-to-string conversion. When `string.byte`
has an explicit end position, a start below the subject is clamped to the
first byte; with no end position, an out-of-range start still returns no bytes.
Array keys exposed by `next`
and the initial and advancing indices of `ipairs` use the same profile split.
The numeric selector of `select` requires an exact integer for numeric values in
Blu and Lua 5.3–5.5. Blu additionally accepts numeric strings using Luau's
truncating conversion; Lua 5.1–5.2 and Luau truncate both numeric and
numeric-string selectors. `select("#", ...)` remains the count form described
above.
`rawlen` is available in Blu, Luau, and Lua 5.2–5.5 but rejected explicitly
for Lua 5.1, which predates it. `typeof` is a Blu/Luau extension; Lua profiles
retain only the shared `type` function.
`string.find` currently implements the common byte-oriented literal-search
slice: relative start indices, empty needles, nil misses, and explicit
`plain=true`. Searches without `plain` support search-relative `^`, subject-end
`$`, byte wildcard `.`, and `%`-escaped punctuation under a fixed pattern-work
limit. The common byte classes `%a`, `%c`, `%d`, `%l`, `%p`, `%s`, `%u`, `%w`,
`%x`, and `%z` are supported, with uppercase class letters selecting their
complements. `%g` and `%G` use graph/non-graph bytes in Blu, Luau, and Lua
5.2–5.5; Lua 5.1 retains its upstream literal-`g`/`G` escape behavior. Bracket
sets support byte literals, ranges, the common classes,
leading `^` negation, and a leading literal `]`. Greedy `*`, `+`, and `?`
repetition and minimal `-` repetition execute through an explicit,
non-recursive backtracking state machine under the same work limit. Captures,
malformed escapes, sets, or repetition, and classes that differ between
targeted dialects are not reinterpreted as literal text; they fail with a
structured unsupported-library-feature error until profile-specific pattern
dispatch is implemented. Returned indices follow the active profile's
legacy-number or modern-integer policy.
`string.find` appends captures after its two indices. `string.match` returns
captures when present and otherwise returns the full matched byte slice.
`string.gmatch` returns a real function iterator. Each invocation resumes from
the previous non-overlapping match, returns captures or the full match, and
advances by one byte after an empty match so iteration terminates. Blu and Lua
5.3–5.5 suppress only a redundant terminal empty match following a non-empty
match that already reached the subject end; zero-width captures such as `()`
still yield the terminal position. Lua 5.1–5.2 and Luau retain that legacy
empty result even after a non-empty terminal match.
Lua 5.1 additionally exposes `string.gfind` as the exact `string.gmatch`
alias; later profiles leave that legacy name absent.
`string.gsub` supports string, numeric, direct table, and function replacements.
Its replacement limit, `string.match` start index, and `string.rep` count use
the active profile's integer conversion: Blu and Lua 5.3–5.5 require exact
integer-representable numbers, while Lua 5.1–5.2 and Luau truncate finite
fractions. Lua 5.2 treats a NaN replacement limit as the default unlimited
bound; Lua 5.1 and Luau truncate it to zero. The optional `string.gmatch`
start index is profile-gated to Blu, Luau, and Lua 5.4–5.5; Lua 5.1–5.3
ignore an extra third argument.
Table replacement keys use the first
capture or the full match, position captures use the active profile's numeric
subtype, and nil or false values retain the original match. Function
replacements receive the captures, or the full match when there are no
captures. Synchronous table `__index` replacement handlers are supported. A
yielding replacement-table `__index` callback remains rejected at the native
`string.gsub` boundary; the pinned Lua 5.1–5.5 and Luau references make the
same choice in the executable corpus. In the owned BluV1 path, a callback may
yield once per match: the callback frame,
match cursor, accumulated output, and replacement count are retained as one
GC-rooted pending operation and resume with the callback's supplied values.
Other yielding library callbacks remain explicit unsupported features.
Nested substring captures and `()` position captures are bounded to 32 and
execute through linked capture events rather than recursive host calls.
`%1` through `%9` match completed substring captures byte-for-byte under the
same work limit. References to absent or unfinished captures fail structurally,
while position-capture references follow the upstream non-match behavior.
`%bxy` matches nested byte pairs without host recursion and charges every
scanned byte against the pattern-work limit.
`%f[set]` implements zero-width byte frontiers, including the virtual zero byte
at each subject boundary, through the same set and work-limit semantics.
`string.gsub` uses the same engine with bounded non-overlapping replacement
and Lua-compatible empty-match progress. In Blu and Lua 5.3–5.5, a redundant
terminal zero-width match is suppressed when a preceding non-empty match
already reached the end of a non-empty subject; the empty pattern and a lone
terminal zero-width match retain their upstream replacements. String and numeric replacements
support literal bytes, `%%`, `%0`, and `%1` through `%9` substring or
position-capture expansion. Lua 5.1 preserves its permissive behavior for
other `%x` replacement escapes by emitting `x`; Blu, Luau, and Lua 5.2–5.5
reject them. The second result is the profile-typed
replacement count. Direct table replacements select by the first capture or
full match; table `__index` handlers use the same bounded callback bridge as
`table.sort`. The owned callback continuation described above preserves this
operation across yields; other yielding callbacks and handlers remain
explicitly pending resumable calls.
`string.format` implements the profile-common unmodified conversion core:
`%%`, `%s` for string or numeric values, Luau/Blu `%*` dynamic value
conversion, `%d`, `%i`, `%u`, `%x`, `%X`, `%o`,
`%c`, `%q`, and default-precision `%f`, `%e`, `%E`, `%g`, and `%G`. `%c`
converts after the active profile's integer rule and wraps modulo 256; Lua
5.1 additionally truncates a resulting NUL byte at its historical C-string
boundary, while later profiles retain the NUL byte. On Blu and
Lua 5.3–5.5, `%q` also emits the literal forms of `nil` and booleans; Lua
5.1–5.2 and Luau reject those non-string/non-number values. Numeric
strings are coerced for the covered integer, floating, hexadecimal, and
character conversions. `%q` quotes
strings with profile-compatible byte escaping; legacy number profiles quote
numeric values, while Blu and Lua 5.3–5.5 use the modern unquoted numeric
form. Luau spells carriage return as `\\r` in `%q`; Blu and Lua 5.3–5.5 retain
the octal control escape. `%a` and `%A` use the modern hexadecimal-float form in Blu and Lua
5.3–5.5 and remain absent from Lua 5.1–5.2 and Luau. Scientific exponents use an
explicit sign and at least two digits as required by the reference runtimes.
One- or two-digit field widths are supported for the implemented conversions,
including the shared `-` flag for left alignment and the numeric `+`, space,
`#`, and `0` flags. One- or two-digit explicit precisions are supported for
integer conversions, `%s`, `%f`, `%e`, `%E`, `%g`, `%G`, `%a`, and `%A`; integer
precision adds leading zeroes and suppresses the `0` field-width flag, while
`%.s` selects zero precision as in the reference grammars. `%.0g`, `%.0G`,
and `%.0a` select one significant digit or the rounded hexadecimal leading
digit.
Integer conversions truncate in Luau and Lua 5.1–5.2 and require an exact
integer representation in Blu and Lua 5.3–5.5. `%s` accepts strings and
numbers everywhere in the covered surface; Blu and Lua 5.2–5.5 additionally
apply the non-yielding `tostring` conversion to tables, functions, and values
with `__tostring`, while Lua 5.1 and Luau retain their non-scalar rejection.
An attempted yielding `__tostring` handler remains rejected at this native
formatter boundary. Output growth is fallible and enforces the hard string
limit. Lua 5.4/5.5 also reject the profile-invalid modifier combinations for
`%d`/`%i`, `%u`, `%x`/`%X`/`%o`, `%s`, `%c`, and `%q`; that matrix is enforced
explicitly, while older Lua profiles and Luau retain their permissive
behavior. The Luau/Blu `%*` extension stringifies one dynamic value without
accepting width or precision modifiers; `*` widths and precisions remain
rejected. Field widths wider than two digits, precisions wider than two digits,
and other unsupported format forms remain structured errors rather than being
approximated.
`string.pack`, `string.unpack`, and `string.packsize` implement the Lua 5.3+
and Luau binary format core in Blu: endian controls, bounded integer widths,
native and explicit floating-point widths, fixed/length/zero-terminated
strings, padding, alignment, and the trailing unpack position. Lua 5.1–5.2
reject these names explicitly; malformed formats, integer overflow, truncated
input, and variable-length `packsize` requests return structured errors. For
unsigned fields whose decoded value exceeds Blu's signed integer range, Blu and
Luau materialize the positive value as a number instead of wrapping it through
`i64`; ordinary in-range fields retain the selected profile's integer/number
model.
Blu and Luau provide `string.split` with a default comma separator,
non-overlapping byte-string separators, retained empty fields, and byte-wise
splitting for an empty separator. Its output table capacity is checked before
allocation. Lua profiles reject the Luau-only function explicitly.
Blu and Luau also provide bounded `table.create` and `table.find`.
`table.create` preflights its array capacity and optionally fills every slot in
Blu/Luau; its preallocated boundary is retained when the last slot is assigned.
Lua 5.5 also exposes the capacity-only form and ignores a fill argument.
`table.find` searches the profile-visible numeric sequence from a positive
optional start, including a numeric hash-start boundary, invokes shared `__eq`
metamethods, and returns a profile-typed index. Other Lua profiles leave these
names absent.
`table.clear` removes array and hash entries without reallocating the table;
when an array allocation exists, later indexed writes retain that allocated
length boundary just like `table.create`, while hash-only tables keep the
compact boundary. Blu and Luau guest tables preserve small hash insertion
order through `table.clone`. `table.clone` performs a bounded shallow copy, so
self-references still point to the source, preserves unprotected metatables,
and carries an explicit preallocated array boundary through the copy.
Protected metatables
produce a structured error as in Luau. Both functions are Blu/Luau-only.
`table.freeze` marks a table shallowly immutable and `table.isfrozen` exposes
that state. Indexed writes, `rawset`, `table.clear`, sorting, and metatable
changes all enforce the same heap-level flag. Freezing twice and freezing a
protected-metatable table fail structurally; shallow clones are mutable.
Legacy `table.getn` is available in Blu, Luau, and Lua 5.1; `table.maxn` is
available in Blu, Luau, and Lua 5.1–5.2. Later Lua profiles leave these names
absent. Blu returns an exact integer from `getn`; `maxn` remains a number
because fractional numeric keys participate in its upstream contract.
The Lua 5.1 `table.foreach` and `table.foreachi` callbacks are available in
Lua 5.1, Luau, and Blu. They invoke callbacks with key/value or index/value
pairs, return the first non-nil callback result, and otherwise return nil.
`foreachi` uses the profile's table-length boundary: Blu keeps its compact
border, while Luau and Lua 5.1 retain the allocated legacy array boundary.
Owned callbacks retain iteration state across yields, including terminal return
calls. In profiles that define it, `pairs` also invokes `__pairs`; owned
handlers have the same resumable operation boundary. Lua 5.2 and 5.3
`ipairs` also invoke `__ipairs`; Lua 5.1, 5.4, and 5.5 ignore that hook.
Owned `__ipairs` handlers use the same bounded callback boundary, including
resumption after an owned yield as a Blu-owned extension; the pinned PUC
references reject that yield across their C `ipairs` boundary.
The pinned mutation slice makes `ipairs` observe a later contiguous index,
while `foreachi` snapshots its starting length. Hash-key visitation order for
`pairs` and `foreach` remains intentionally unspecified. Clearing the current
key during `next`, `pairs`, or `foreach` preserves a valid continuation,
including after collection; a key that was never present still reports the
standard invalid-iteration-key error. Large hash tables retain an internal
key-position index so repeated `next` calls remain bounded without promising
an observable hash order; deleted entries remain invisible while a deleted
current key remains a valid continuation token.
Legacy `gcinfo` is available in Blu, Luau, and Lua 5.1 and reports the
runtime's accounted live memory in whole KiB. Blu returns an integer; the
number-only compatibility profiles return a number. Lua 5.2–5.5 leave the
removed function absent.
`coroutine.running` follows the active profile: Lua 5.1 returns nil on the main
thread, Luau returns only the thread, and Blu/Lua 5.2–5.5 also return the
main-thread boolean. `coroutine.isyieldable` is true on Luau's main thread,
false on the Blu/Lua 5.3–5.5 main thread, true inside their coroutines, and
explicitly unsupported for Lua 5.1–5.2 where it is absent. Lua 5.1's legacy
`math.mod` name is profile-gated as an alias of `math.fmod`; Blu, Luau, and
Lua 5.2–5.5 keep only `math.fmod`.
Lua 5.4/5.5 `coroutine.close` explicitly unwinds pending owned `<close>` values
from new or suspended coroutines; successful cleanup returns `true`, while a
handler error returns `false` and the error value. The coroutine is dead after
either result. A pinned abandoned-coroutine probe also records that collection
does not implicitly run the pending handlers. A close handler that attempts to
yield is rejected at this non-resumable boundary with `(false, error)` and the
thread is still dead. Other reclamation/finalizer interactions remain outside
the owned root model.
Owned BluV1 coroutine entry paths use an iterative activation trampoline for
nested `coroutine.wrap`/`resume` chains. It retains each parent/child thread,
arguments, delivered values, status transition, and GC roots without recursive
host re-entry; the pinned Lua 5.1 portable child matrix now passes all eight
cases, including `sieve.lua`. A bounded 64-level guard remains on the legacy
or foreign synchronous bridge, and a suspended Blu continuation that would
resume an arbitrary cross-thread activation still reports a structured
unsupported-feature boundary. The child runner keeps each official case
isolated from the conformance parent while preserving those precise results;
this is not a blanket claim for native callback or foreign-coroutine
continuations.
`collectgarbage` supports the shared `collect` and `count` commands. Lua 5.1–5.5
also support `stop` and `restart`, returning zero; Lua 5.2–5.5 additionally
support `isrunning`, which reports the automatic-collection state. Lua 5.1–5.4
support `setpause` and `setstepmul`, returning the previous integer parameter;
Lua 5.5 exposes the corresponding `param` interface for `pause` and `stepmul`.
Lua 5.2, Lua 5.4, and Lua 5.5 expose the profile-appropriate
`generational`/`incremental` controls; Lua 5.1 and Lua 5.3 keep those commands
unsupported. All Lua profiles support `step`, which performs a bounded full
collection and reports success. Stopping automatic collection does not disable
an explicit `collect` or `step`. Collection traces active frames, globals,
threads, upvalues, and host-retained values. `count` reports the runtime's
accounted live GC-heap kibibytes; it is not presented as whole-process memory.
Pinned Luau returns no values from `collect`; Lua profiles return zero using
their legacy-number or modern-integer policy. Blu and Luau intentionally expose
only `collect` and `count`; unsupported control commands fail explicitly. When
automatic collection is stopped, allocation paths do not trigger opportunistic
collection and can instead report the configured heap-object limit.
The owned collector closes unreachable host-backed IO handles and traces a
metatable attached to those opaque userdata values. Lua 5.1–5.5 host-created
opaque userdata can receive a bounded guest `__gc` callback through
`debug.setmetatable`; the callback runs before host resource release. Lua
5.2–5.5 table
metatables with `__gc` now retain the table through collection, invoke a
bounded callback once in reverse registration order, and preserve a table
resurrected by that callback; Lua 5.2–5.3 propagate the first finalizer
error/yield as a collection failure, while Lua 5.4–5.5 discard that failure and
continue. Blu, Luau, and Lua 5.1 keep table finalizers unavailable. Guest-side
userdata allocation beyond Lua 5.1's `newproxy` remains unimplemented. A trusted native library bridge may
create bounded opaque host-owned userdata handles, and the existing IO bridge
is another source of such handles; payloads never become guest-readable. Lua
5.3–5.5 can
explicitly re-arm a resurrected table or host userdata from its `__gc` callback;
Lua 5.2 retains its one-shot finalizer state. Pinned PUC probes confirm the
host-userdata finalizer order, resurrection/rearming, and error/yield policy;
full finalizer ordering and abandoned-thread reclamation interactions remain
explicit compatibility gaps; pinned no-implicit-close behavior and explicit
Lua 5.4/5.5 `coroutine.close` cleanup are covered.
The boundary probe records the matching table-finalizer scheduling and
resurrection behavior for Lua 5.2–5.5, including Lua 5.3–5.5 re-arming from
`__gc`, reverse-order dispatch, and the profile-specific error/yield policy.
The owned collector derives a conservative read/lifetime mask for active Blu
frames, retaining active locals, closure captures, and all registers reachable
through backward control flow while allowing dead temporaries to be collected.
This closes the pinned Lua 5.3–5.5 re-arm cycle; compiler-emitted explicit
liveness metadata remains a future optimization. Lua 5.4 additionally exposes
`debug.setcstacklimit`; the owned runtime keeps that control absent rather than
mapping it to the guest call-depth limit, because Rust stack capacity and Lua's
C-stack limit are different authorities.
`table.sort` supports bounded default ascending order for uniform numeric
sequences without NaN and uniform byte-string sequences. It returns no values
and accepts an omitted or nil comparator. Numeric sorting uses exact mixed
integer/number ordering without `f64` round-trip loss. Custom comparator
callbacks and `__lt` metamethod ordering use the bounded callback bridge;
owned callbacks retain insertion-sort state across yields, including GC roots
and terminal return calls.
`table.pack` and `table.unpack` are available in Blu, Luau, and Lua 5.2–5.5;
Lua 5.1 leaves those table-library names absent. The legacy global `unpack`
is available in Blu, Luau, and Lua 5.1–5.2 and is absent in Lua 5.3–5.5.
`table.move` performs bounded overlap-safe moves and returns the destination
table for Blu, Luau, and Lua 5.3–5.5. Lua 5.1–5.2 leave the name absent because
those libraries do not define it.
Default `table.concat` and `table.unpack` ends use the same profile-specific
length boundary; explicit end positions remain independent of it.
An explicit `nil` for either optional `table.unpack` bound selects its default,
matching the pinned Luau and Lua optional-argument convention. Blu and Luau
reject an unpack request that would produce more than 7,999 result values,
matching the pinned Luau boundary; Lua 5.2–5.5 retain their native larger
result behavior.
`rawlen`, `table.getn`, `table.insert`, `table.remove`, and `table.sort` use
that same boundary for their profile. `table.maxn` continues to report the
maximum numeric key, including an isolated high key; `table.find` preserves
the pinned Luau/Blu sparse search behavior.
The boundary also preserves the constructor-versus-assignment split: legacy
profiles do not promote holes created by writes to an initially empty table,
Lua 5.5 promotes a contiguous `2,3` assignment run, and Blu/Luau keep the
high-only or nil-write boundary covered by their official sparse-table probes.
The optional indices for `table.concat` and `table.unpack`, and the three
positions for `table.move`, use exact integer-representable conversion in Blu
and Lua 5.3–5.5 and truncating conversion in Lua 5.1–5.2 and Luau. `table.create`
uses the same split for its count in Blu and Lua 5.5, while Luau truncates its
count.
Blu keeps signed 64-bit `table.move` positions; the Luau reference's signed
32-bit destination-wrap diagnostics are consequently isolated when running
the official Luau corpus under the Blu profile.
The positional arguments of `table.insert` and `table.remove` follow the same
split, and Luau/Blu-only `table.find` truncates its optional start in Luau while
Blu requires an exact integer-representable start.
For Blu and Luau, an integral `table.insert` position outside `1..#t+1` is
stored as a raw keyed write; legacy Lua profiles retain their range error.
Blu and Luau also treat non-finite explicit positions (NaN and ±infinity) as
a successful no-op.

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

The initial shared math library includes `math.fmod`, `math.modf`,
`math.pow`, `math.frexp`, `math.ldexp`, `math.random`, and `math.randomseed`.
`math.fmod` requires two numeric arguments and uses truncating remainder
semantics, so the result follows the dividend's sign and is intentionally
distinct from the language `%` operator. Blu and Lua 5.3–5.5 preserve
all-integer inputs and reject an integer zero divisor; Luau and Lua 5.1–5.2
use number semantics, including NaN for a zero divisor.

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
a structured limit error. Embedders can clone `Vm::interrupt_handle` and
request interruption safely from another thread. Both bytecode engines observe
the persistent signal at instruction boundaries and return
`RuntimeError::Interrupted`; resetting the handle permits later execution.
An absolute `std::time::Instant` deadline can independently be installed with
`Vm::with_deadline` or `Vm::set_deadline`; expiration returns
`RuntimeError::DeadlineExceeded`, and clearing it permits later execution.
This is cooperative VM interruption, not preemption of a currently running
native callback. Blocking or long-running host functions must honor
cancellation through their own declared contract. A callback can call
`Vm::check_execution` between bounded work units to observe both interruption
and deadline expiry. `Vm::active_semantic_profile` reports the executing
caller's artifact profile inside a callback, rather than the VM's fallback
dialect, so one host binding can dispatch explicit Blu/Luau/Lua behavior.
Output growth is preflighted and uses fallible
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
maps before insertion. Live task states, including the main thread, have an
independent configurable bound through `Vm::with_task_limit`. Coroutine
creation first collects unreachable thread handles while rooting the proposed
function and active state, then returns `RuntimeError::TaskLimit` without
installing a partial coroutine if the bound remains exceeded.
Native-function and global registries have configurable
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
string-result limit. Direct-copy operations including `tostring`,
`string.sub`, `string.match` captures, `string.reverse`, `string.lower`,
`string.upper`, and `string.split` validate that same bound before copying
even when an oversized input string originated in host code.
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
per-VM cache, circular-load detection, GC-rooted module results, and Lua-family
owned `package.loaded`/`package.preload` tables. `require` invokes a configured
Lua-family `package.preload[name]` function before consulting the host loader,
caches its first result (or `true` when it returns no value), and passes the
module name as its first argument. Lua-family owned profiles also expose
customizable `package.searchers` (and the Lua 5.1-compatible `package.loaders`)
tables; `require` dispatches through the profile-selected table, with bounded
preload and host-loader searchers installed by default. Searchers may return a
loader plus one extra value, which `require` forwards to the loader. Owned Lua
5.1–5.5 profiles can suspend and resume guest searchers and their returned
loader callbacks across coroutine yields, retaining the module name, selected
loader, and `package.loaded` roots until completion. This is an owned
continuation extension: pinned Lua 5.1–5.5 references reject the same loader
yield across their native `require` boundary. Lua 5.1–5.5 also expose the
platform-neutral `package.config` string with the upstream layout: Lua 5.1 uses
`/\n;\n?\n!\n-`, while Lua 5.2–5.5 append a final newline. Blu and Luau keep the
package table hidden from guest code.
When an exposed Lua profile is initialized, its standard-library identities are
also installed in `package.loaded` (`_G`, `package`, `coroutine`, `table`, `io`,
`os`, `string`, `math`, `debug`, and `utf8` where present). Consequently,
`require` of those names returns the same table as the corresponding global;
the hidden Blu/Luau `package` and `debug` surfaces remain unavailable rather
than being fabricated by `require`.
Owned Blu and Lua
5.1–5.5 profiles additionally expose `loadfile` and `dofile` through an
explicit embedding `Vm::set_file_loader` callback. The callback is the
authority boundary: the runtime does not read host paths directly. The
conformance child runner's loader and probe are additionally rooted to the
fixture directory and reject absolute or parent-traversing paths. An
unconfigured loader returns a structured unavailable-capability error.
`loadfile` uses the same profile-aware source loading and mode checks as
`load`, while `dofile` loads and immediately executes the returned chunk.
Luau keeps both names hidden in the owned library surface. Lua 5.2–5.5 expose
`package.searchpath` through the same style of explicit `Vm::set_file_probe`
callback; Lua 5.1 does not define that function. The probe checks candidate
paths without reading file contents, and an unconfigured probe returns a
structured unavailable-capability error. Lua 5.1–5.5 initialize `package.path`
and `package.cpath` with the pinned PUC Unix or Windows defaults; embedders may
override them through guest assignment or `Vm::set_package_path` and
`Vm::set_package_cpath`. These strings describe search templates only; ambient
native library loading remains outside the VM and requires an explicit trusted
bridge. With default or guest-
configured `package.path` and both the file loader and path probe, owned Lua
profiles add a source-backed `require` searcher; an embedding-supplied module
loader retains precedence. The compiled module receives its module name as
`...`. Not-found errors preserve the profile's Lua-style attempted-path
diagnostics, including the different leading-newline convention used by Lua
5.2/5.3 versus Lua 5.4/5.5. `package.loadlib` is present in Lua 5.1–5.5 and returns the standard
three-result unavailable boundary (`nil`, an error string, and `"absent"`) until
an explicit versioned native bridge is supplied. A trusted embedding may
install `Vm::set_native_library_loader`; that callback owns library resolution,
ABI/version checks, symbol binding, and any host resource lifetime. It may
return a guest-callable value or a bounded opaque userdata handle created by
`Vm::create_userdata`; the VM never interprets the handle payload. A bridge
that needs standard Lua failure returns can use
`Vm::set_native_library_loader_result` to return a bounded `(nil, message,
where)` result with `open`, `absent`, or `init` status. Lua 5.1's guest
`newproxy` primitive is also implemented with the same bounded opaque userdata
and finalizer machinery; later Lua profiles omit it.
The VM performs no ambient dynamic loading, and other system-library surfaces
remain outside this slice.

Lua 5.1 and Lua 5.2 additionally expose the legacy `module` global and
`package.seeall`. `module(name, ...)` creates or reuses the corresponding
`package.loaded` table, publishes the dotted namespace in the caller's global
environment, and fills `_M`, `_NAME`, and `_PACKAGE`; Lua 5.1 rebinds the
calling owned closure environment and returns no value, while Lua 5.2 returns
the module table without rebinding the lexical environment. `package.seeall`
installs the standard global-environment `__index` fallback. Lua 5.3–5.5 omit
both legacy members. This compatibility slice remains separate from native
module loading and does not provide guest userdata finalizers.

Lua 5.1–5.5 also expose `os.clock`, `os.difftime`, and `os.getenv`.
`os.difftime` is a pure numeric operation. `os.clock` requires the host to
install `Vm::set_clock_getter`, which owns the clock policy and returns a
nonnegative finite value in seconds. `os.time()` requires the host to install
`Vm::set_time_getter`, which supplies Unix seconds and returns a profile-correct
numeric subtype. `os.date` forwards a bounded format and optional timestamp to
`Vm::set_date_getter`, which owns locale and timezone policy; table-returning
calendar forms additionally use an explicit structured calendar capability.
`os.getenv` returns `nil` until the host installs
`Vm::set_environment_getter`, which is the authority boundary for environment
names and values; Blu and Luau keep the `os` table hidden from guest code.
The bounded debug slice exposes `debug.getmetatable`, `debug.setmetatable`,
`debug.getregistry`, function-targeted `debug.getinfo(f, "Snu")`, bounded
active-owned-frame or suspended-owned-coroutine `debug.getinfo(level, "Snu[f]")`, the
level-zero C-function record for `debug.getinfo(0, "Snulf")`, and active-frame
`debug.getinfo(level, "l").currentline` and `debug.getinfo(..., "L").activelines`
for Lua 5.1–5.5 when BluV2 per-PC line metadata is available.
The registry is a dedicated, GC-rooted VM table and is never the
guest global environment; metatable access still bypasses a protected
`__metatable`. `getinfo` reports real function kind/source identity, definition
line range, and parameter, vararg, and upvalue counts; direct global, local,
field, and method call sites also retain their active `name`/`namewhat` pair.
Lua 5.1 retains its omission of the
later `nparams`/`isvararg` fields. Luau retains a `debug` table with the
bounded `traceback` member but not the other members listed above, while Blu
hides the global `debug` table. Function definition line
ranges are retained by newly compiled BluV2 artifacts; legacy BluV1 artifacts
decode with zero line metadata. `debug.traceback` formats bounded active-owned
frames, `debug.getlocal(level, index)` reports active or suspended owned local names,
and `debug.setlocal` updates the current, retained caller, or suspended owned frame.
Retained caller frames exclude tail calls, matching Lua's tail-call frame elision.
`debug.getupvalue`/
`debug.setupvalue` report and update owned closure cells, while Lua 5.2–5.5
`debug.upvaluejoin` can make two owned closure slots share the same cell.
The same profiles expose `debug.upvalueid` as a stable lightuserdata identity
(whose guest `type` is `"userdata")); Lua 5.1 retains the historical
absence of both APIs. Heap-owned `io.lines()` C-like iterator closures also
match the profile split: Lua 5.1 hides their upvalue, while Lua 5.2–5.5 expose
the unnamed file-handle upvalue and its stable identity; their C metadata also
reports the pinned 2-versus-3 captured-upvalue count, and joining that foreign
slot remains rejected. `debug.sethook`/
`debug.gethook` support non-yielding line callbacks
(`"l"`) with real per-PC source lines and instruction-count callbacks. Owned
call/return events, profile-specific tail-call events, and host-native callback
call/return events from owned frames, including hooks targeted at a specific
owned coroutine thread, are supported; host-native callback frames
report the stable C-frame shape (`what`, source, arity, vararg metadata, and
retained direct call-site names), while special native-frame metadata,
foreign-running-thread frames, and
legacy-continuation locals remain isolated; yielding from a hook is rejected at
the C-call boundary, matching the pinned PUC runtimes.
Legacy Lua 5.1/5.2 `module` option callbacks have the same explicit boundary:
an option that yields is rejected, and the partially mutated module namespace
is not retained as a continuation. The owned/reference result is covered by
the conformance corpus.

## Remaining capability-bound library gaps

The current owned surface deliberately keeps the following gaps explicit:

- Lua 5.1–5.5 now expose a bounded `io.open`/`io.type`/`io.close` slice backed
  by non-forgeable, garbage-collected userdata and an explicit
  `Vm::set_io_file_opener` callback. Blu and Luau keep `io` hidden. The host
  opener owns path and mode policy; without it, `io.open` returns the standard
  `(nil, message)` shape, and a configured opener failure is mapped to the
  same recoverable result while preserving a raised host value or bounded
  error string. File-handle `read` (`*a`, `*l`, `*L`, and bounded
  byte-count forms), `write`, `seek`, and `flush` are now forwarded through
  optional `IoFile` capabilities and exercised against the pinned PUC
  references. `file:setvbuf` forwards bounded `no`/`full`/`line` buffering
  requests and an optional size through the host file capability. The read
  bridge accepts one or more bounded requests per call,
  and `file:lines()` provides a bounded line or numeric iterator, including
  multiple formats per iteration, whose opaque handle root is released at EOF
  or explicit close. An explicit
  `Vm::set_io_stream_opener` grant now exposes stable `io.stdin`, `io.stdout`,
  and `io.stderr` handles plus the default `io.input()`/`io.output()`/
  `io.read`/`io.write`/`io.flush` paths. String arguments to `io.input` and
  `io.output` open through the file capability and become the current stream;
  unlike `io.open`, a configured filename-opener failure is raised by
  `io.input`/`io.output`, matching the PUC `opencheck` boundary. Passing a
  closed opaque file handle to either stream setter is also rejected before
  changing the current stream. Rebinding a default stream leaves the previous
  handle usable until it is explicitly closed or collected; an argument-less
  `io.close()` targets the newly selected current output handle.
  `io.tmpfile()` is exposed through a separate explicit
  `Vm::set_io_tempfile_opener` grant, preserving opaque userdata identity,
  `io.type` behavior, and guest finalizer behavior; without that grant it
  reports the standard unavailable-capability error. A configured opener
  failure is recoverable and returns `(nil, error)` while preserving a raised
  host error value through the bounded error conversion.
  `io.popen(command, mode)` is exposed through a separate
  `Vm::set_io_popen_opener` grant. The callback owns process/pipe execution,
  mode policy, resource limits, and cleanup; without it the function reports
  an unavailable-capability error, and the VM never spawns a process itself.
  A configured opener failure likewise returns `(nil, error)` rather than
  escaping as a VM failure; invalid arguments and mode policy remain
  argument-boundary behavior.
  Global `io.lines()` accepts the default input stream or a filename through
  the corresponding capability callbacks. Multiple read formats are forwarded
  in one call, while `*n` requires the optional `IoFile::read_number` host
  capability and converts the returned token under the active profile's
  numeric rules. `io.lines()` iterators are heap-traced callable closures, so
  a file userdata discarded with an unused iterator now runs its `__gc`
  callback in the same collection cycle as the pinned Lua 5.1–5.5 references.
  Host failures from `file:read`/`io.read`, `file:write`, `file:seek`,
  `file:flush`, `file:setvbuf`, and explicit `io.close` use the recoverable
  `(nil, error)` file-result shape. The handle is marked closed before the
  host close callback runs, so a recoverable close error cannot be retried;
  an `io.lines` iterator read failure raises the host error, matching the PUC
  iterator boundary. Missing optional
  operation capabilities remain explicit unavailable-capability errors. The
  filename form of `io.lines` returns one result through Lua 5.3 and the
  pinned four-result `(iterator, nil, nil, file)` shape in Lua 5.4–5.5, with
  the returned file closed on EOF or generic-for cleanup.
  Their profile-specific `debug.getupvalue`/`debug.upvalueid` behavior is
  covered by the same differential surface.
  Table-backed pseudo-handles are not used because they would be guest-
  forgeable and would not preserve Lua handle identity or close semantics.
- `os.clock`, no-argument `os.time`, and callback-backed `os.date` are exposed
  for Lua 5.1–5.5 through host-authorized callbacks. Formatted dates forward
  the bounded format and optional timestamp; `os.date("*t")` and
  `os.date("!*t")` additionally materialize a validated `CalendarDate`
  supplied by an explicit host calendar callback, preserving Lua's `wday`,
  `yday`, and `isdst` conventions. `os.time(table)` forwards validated
  `CalendarDateInput` fields—including Lua's default noon/zero time fields and
  optional `isdst`—to a separate host reverse-calendar callback. The host owns
  timezone resolution, DST policy, and normalization. Filesystem mutation,
  process execution, exit, locale, and temporary-name
  operations likewise require separate explicit capability callbacks and
  platform policy.
- Lua 5.1–5.5 expose `os.remove` and `os.rename` through separate explicit
  host callbacks. A configured callback returns Lua's successful `true`
  result; a callback failure returns Lua's recoverable `(nil, error)` shape,
  preserving raised host values or bounded error strings. Without the
  callback, the VM reports a structured unavailable capability error rather
  than mutating the host filesystem. The pinned success and failure paths are
  differentially covered.
- Lua 5.1–5.5 also expose `os.execute` through an explicit host process
  callback. The callback reports shell availability or a bounded command
  result; the VM maps Lua 5.1's numeric status convention separately from the
  Lua 5.2–5.5 `(success, kind, code)` convention. No process can run without
  the callback, and shell quoting, resource limits, isolation, and failure
  message shaping remain host policy.
- Lua 5.1–5.5 expose `os.exit` through an explicit host termination callback.
  Lua 5.1 forwards a numeric status; Lua 5.2–5.5 additionally accept boolean
  status values and forward the truthiness of the close-before-exit argument.
  A successful callback returns control to the embedding VM for testability;
  the VM never terminates the process itself.
- Lua 5.1–5.5 expose `os.setlocale` and `os.tmpname` through explicit host
  callbacks. Locale category selection and process-global locale effects stay
  with the host; rejected locale requests return `nil`, and temporary names
  are bounded before entering the guest. No locale mutation or name generation
  occurs without the corresponding capability.
- `debug` currently stops at raw table and host-backed userdata metatable access, the dedicated
  registry table, function-targeted `getinfo(..., "Snu")`, active owned
  closure stack-level `getinfo(..., "Snu[f]")` plus current coroutine-thread
  targets, bounded `getlocal`, bounded
  traceback, non-yielding line/count hooks, owned call/return/tail events, owned upvalue access, and profile-correct
  userdata-value access for Lua 5.2–5.5. Lua 5.2/5.3 retain one table-or-nil
  uservalue slot; Lua 5.4/5.5 honor indexed slots and return the standard
  success flag, while modern opaque file handles intentionally have zero
  slots. Other line-dependent options, non-current active legacy-continuation or
  foreign-running coroutine-thread locals,
  special native-frame metadata, foreign hook events, foreign-closure upvalue
  identity and exact pointer formatting require a
  specified public stack/debug model. Pinned PUC probes report active caller
  names (`global`/`method`) and a level-zero `debug.getinfo` record. The owned
  model now implements that level-zero C record (including its `"f"` result) and
  recognizes retained local, global, field, and method call sites; dynamically
  constructed field targets report the standard `field`/`?` pair. The main
  chunk now reports its `main` metadata shape and a rooted function object for
  the `"f"` option. Exact pointer formatting and recursive invocation through
  that returned object remain isolated;
  several of those APIs also depend on userdata support. The Lua 5.1–5.5 and
  Luau/Blu presence choices are recorded by executable profile tests, but the
  omitted members remain unsupported rather than call-time stubs.
- `string.dump` is exposed for owned Lua 5.1–5.5 closures. It emits a bounded,
  canonical Blu binary artifact whose selected function prototype becomes the
  dumped chunk's main and whose descendants are remapped safely; the existing
  binary `load` path round-trips the result. The second boolean argument emits
  a stripped BluV1 artifact with debug locals, upvalue names, line ranges, and
  per-PC lines removed. Captured upvalue values are never serialized: Lua 5.1
  reloads them as nil, while Lua 5.2–5.5 seed the first reloaded upvalue from
  the supplied chunk environment and leave later slots nil. The bytes are
  Blu's validated artifact format, not a claim of PUC Lua byte-for-byte
  compatibility; foreign Lua binary chunks and exact dumped-byte identity
  remain isolated. Pinned `luac` probes produce versioned Lua headers (`Q`,
  `R`, `S`, `T`, and `U` for Lua 5.1 through 5.5) plus version/ABI-dependent
  constant and instruction encodings, so accepting them requires a separate
  bounded decoder and an explicit cross-version policy.
- `package.loadlib` remains an unavailable-result boundary until a versioned
  native bridge is installed. The bounded bridge can return native callbacks
  and opaque host-owned userdata, but it does not expose Lua's C stack, raw
  allocator, `lua_State`, or the C-side `newuserdata` primitive. Lua 5.1's
  guest `newproxy` is covered separately. ABI-compatible C modules, dynamic
  loading, and isolation policy are separate from source/module search.
Portable
V1 envelopes canonically declare identity, dialect, bytecode versions,
imports, exports, schema digests, and authority requirements; decoding is
bounded, integrity-checked, and validates the contained bytecode without
executing it. `Engine::execute_package` now admits a package whose authority
profile is covered by the host policy when every declared capability has an
exact host grant for the same opaque name and scope. This is a policy gate,
not a capability handle: delegation, attenuation, revocation, auditing,
filesystem resolution, and effectful bindings remain future work. The public
engine still executes only dialect-matched packages with no imports; required
service linking, dependency locks, signatures, LuaRocks resolution, and native
module loading fail explicitly where exposed.

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

The `blu-conformance` runner has opt-in source-level upstream inventory gates:
`--official-luau-tests <checkout>` runs the portable Luau conformance subset,
`--official-luau-profile <blu|luau|both>` selects which owned profile(s) to
execute for that gate, and `--official-lua-tests <checkouts>` runs the pinned Lua 5.1 `test/` subset
with captured standard streams. Each gate reports reference-side harness
isolations separately from Blu compile/runtime/output deviations; a passing
reference command is never silently treated as a Blu pass when the owned
frontend or host capability differs.
The Lua gate executes each Blu case in a separate child process so an isolated
fixture failure or hard limit cannot exhaust the conformance parent. The pinned
Lua 5.1 portable matrix currently passes 9/9 cases in Blu, including its deep
`sieve.lua` coroutine chain. At the pinned Luau revision, the runner also
instruments `assert` and reports the failing assertion ordinal. The current
Blu-owned ledger is therefore concrete rather than a blanket suite waiver:
the selected portable Lua 5.4.8 and 5.5.0 matrices each execute 16 cases,
with 9 reference passes and 7 explicitly isolated cases. The original
portable smoke subset remains 8/8 for each version, including the 5.5
named-vararg fixture; the added cases are retained to make the remaining
frontend, continuation, diagnostic, and library boundaries executable.
The modern runner executes each fixture with a
fixture-rooted file capability and an explicit portable child option, so these
results exercise the source-backed `require` and path-search behavior rather
than relying on ambient host files. The pinned Luau corpus currently contains
34 selected fixtures and has 25 reference passes in each owned profile. Blu
has 12 profile-isolated probes: the modern `coroutine.running` pair and
main-thread yieldability, the bounded-versus-full `debug` surface, Blu's
semicolon-only syntax acceptance, Luau's double-number comparison at `math`
assertion 33, its signed 32-bit `table.move` destination wrap, the `pcall`
traceback assertions that require `debug`, Blu's hidden `os` library at
`sort`/`os.clock`, Luau's typed iterator diagnostic in `iter_fenv`, Blu's
structured nil-call diagnostic in `tmerror`, Luau's canonical `__tostring`
return diagnostic in `events`, Luau's surrogate-codepoint
rejection at `utf8` assertion 327, and the documented negative-zero and
mixed table-hash-order boundaries in `basic`. The Luau-owned profile has no
profile-isolated probes in this selected suite. Blu
continues to accept Luau's numeric-string
`select` selectors while retaining exact numeric-selector conversion for
non-string values. The semicolon's syntax-error result is otherwise exposed
through the standard `[string "..."]:line:`-shaped boundary. The standalone
Luau reference harness adds 9 environment isolations: fixed `./` chunk-path
spelling in `basic`, `closure`, `debug`, and `pcall`; the worker-thread
execution difference in `coroutine`; the absent C++ `cYieldingIterator`
callback in `iter`; the harness's `_G` mutability choice in `pm`, `tables`,
and `vararg`; and the post-yield source-name spelling in `pcall`. The native
assert wording, Luau call/concat/iterator diagnostics, same-delimiter balanced
patterns, userdata `__len`, and numeric-for binding state now have executable
owned regressions.
The typed Luau function/local/vararg annotation slice is now parsed and erased
for both Blu and Luau, including qualified names, simple optional suffixes,
and balanced table/generic/function-type containers. The owned `pcall.luau`
fixture now passes end-to-end in the Luau profile, including its deep
protected-stack result-arity and post-yield error cases; a bounded register
snapshot is taken only at coroutine creation/native-debug boundaries so this
coverage does not impose a permanent recursion-time allocation cost. Running
that fixture under the default Blu profile remains an explicit library-surface
isolation because its traceback-handler assertions require `debug`, which Blu
intentionally hides.
The official `tables.luau` fixture's semantic table cases pass in both owned
Blu and Luau profiles, including its table.create capacity boundary,
hash-start/table.find equality, current-key deletion, lightuserdata keys,
constant-table iteration order, out-of-range/non-finite `table.insert` cases,
high-only sparse length, and small-hash clone order. Its 65,535-iteration by
16-bit sparse-array allocation stress follows a 10,000-global setup and
completes under the explicit 60-second/40-million-instruction watchdog in
both profiles. The
official child aliases `bit32` lexically while the fixture clears lowercase
`_G` entries, matching the C++ harness's built-in lookup shape. Focused
table.create, table.find, same-table key/state, current-key deletion,
scalar-global GC, unpack-boundary, clone-order, constant-field ordering, and
Blu/Luau insertion regressions remain in the workspace suite.
The complete `strings.luau` fixture now passes in the Blu profile, including
Luau's `%q` control-character spellings and dynamic-format cases.
The official `literals.luau` scanner fixture now passes in both owned profiles;
generic-for coroutine iterators retain conservative register roots across the
nested `loadstring` allocation/collection stress so a live iterator cannot be
reclaimed by precise backward liveness.
Each owned Luau fixture runs in a dedicated child with a 30-second cooperative
VM deadline and a parent process watchdog; `tables.luau` gets an explicit
60-second/40-million-instruction budget for its bounded sparse stress, so work
that does not return to the VM (for example a long native/table operation)
cannot block the rest of the matrix. The pinned release conformance runner now passes the full upstream `bitwise.luau`
fixture for both Blu and Luau profiles; the focused `bit32` extract/replace
and protected-call cases remain permanent regression coverage.

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
