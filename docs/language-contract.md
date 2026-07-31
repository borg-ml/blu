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
otherwise they use the VM global registry. Semicolons are retained tokens and
act as optional statement separators or empty statements, including after
`return`. Lua 5.3--5.5 artifacts store literals through `i64::MAX` as exact
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
The shared numeric-for slice accepts
`for name = initial, limit [, step] do ... end`. Controls are evaluated exactly
once and copied into hidden registers before the loop variable enters scope.
The implicit step is positive one in the profile's number representation.
Explicit steps must currently be provable numeric literals; positive and
negative directions are lowered explicitly. Literal zero follows the pinned
split: Lua 5.1–5.3 and Luau classify it with non-positive steps. Lua 5.4–5.5
reject zero during lowering with `BLU-COMPILE-0004`, preserving their upstream
"`for` step is zero" failure, and Blu uses the same diagnostic because its
behavior remains unassigned. Dynamic steps are evaluated once and select their
direction at runtime for Luau and Lua 5.1–5.3. Blu and Lua 5.4–5.5 reject
dynamic steps with `BLU-COMPILE-0003` because they can reach an unassigned or
erroring zero case.
The owned generic-for slice accepts
`for name [, name ...] in expression [, expression ...] do ... end` in every
profile. Its expression list is evaluated once and adjusted to the iterator,
state, and control triplet; Lua 5.4/5.5 additionally retain the fourth
to-be-closed control. The final call supplies remaining controls through
bounded fixed MULTRET. Each step calls the iterator with state and control,
binds its fixed results, and terminates only when the first result is `nil`.
`break` and profile-available `continue` use structured loop scopes. The
owned compiler also parses Lua 5.4/5.5 `<const>` and `<close>` local
attributes, rejects const writes, and executes `__close` on normal scope exit,
`break`, return, `goto`, and protected errors. Error objects are passed to
handlers, reverse-order cleanup continues after a handler error, and yielding
handlers resume through owned coroutine continuations. Full finalizer/GC and
abandoned-coroutine semantics remain unimplemented.
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
function-targeted `getfenv`/`setfenv`. Owned `load` also accepts a reader
function and concatenates its bounded string chunks; an empty string terminates
the reader as in the reference runtimes. Yielding readers, binary chunks, and
exact mode-string behavior remain unsupported. Stack-level environment
rebinding and 5.5 declaration modes remain unsupported.
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
source-order commits begin. This preserves simultaneous assignment and the
pinned `value[1], value = replacement, other` behavior. Local declarations
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
string length. Lua 5.1 ignores table `__len` and therefore remains raw. Blu,
Luau, and Lua 5.2–5.5 resumably invoke a present closure/native handler and
store its first result without applying raw-length numeric conversion.
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
value and reports the main thread as yieldable; Blu follows modern Lua by
returning `(thread, is_main)` and making the main thread non-yieldable. Owned
BluV1 coroutine entry closures now have a native continuation representation:
direct `coroutine.yield` calls resume repeatedly, preserve captured state, and
remain GC-rooted while suspended. Native library operations that invoke
yielding callbacks still require operation-specific continuations and remain
explicit unsupported features.

The owned standard-library slice now includes profile-gated `utf8.len`,
`utf8.codepoint`, `utf8.char`, `utf8.offset`, `utf8.codes`, and
`utf8.charpattern` for Blu and Lua 5.3–5.5. `utf8.offset` follows the
byte-boundary rules of the selected reference; Lua 5.5 additionally returns
the final byte position. `utf8.codes` is a bounded stateful iterator over byte
positions and code points.
Invalid UTF-8 is reported through the Lua-compatible `utf8.len` result pair;
invalid sequences passed to `utf8.codepoint` and invalid Unicode scalar values
passed to `utf8.char` remain structured library errors. Lua 5.1–5.2 do not
expose this global. Filesystem, native-module, yielding-searcher, and other
system-capability library surfaces remain explicitly incomplete.

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
in every profile, return NaN outside their real domains, and reject non-number
arguments structurally. `math.floor` and `math.ceil` return numbers for Luau
and Lua 5.1–5.2. Blu follows Lua 5.3–5.5 by returning exact integers when the
rounded value fits `i64`, retaining a floating result for finite out-of-range
values, infinities, and NaN. `math.modf` uses the same profile split for its
truncation-toward-zero integral result and returns the signed fractional part
as a number.
`math.abs` likewise preserves integer inputs in Blu and Lua 5.3–5.5, including
the upstream wrapping minimum-integer result, and returns numbers elsewhere.
Lua 5.1 ignores extra `math.log` arguments; Blu, Luau, and Lua 5.2–5.5 use the
second argument as the logarithm base.
`math.min` and `math.max` retain the selected operand's integer subtype in Blu
and Lua 5.3–5.5, return numbers in legacy profiles, and use upstream ordered
selection so NaN does not silently replace or get replaced by another operand.
Mixed integer/number selection uses the same exact full-range comparison as
source operators.
`math.type`, `math.tointeger`, and unsigned integer comparison `math.ult`
follow the Lua 5.3–5.5 contracts in those profiles and Blu. They fail with an
explicit unsupported-profile error in Luau and Lua 5.1–5.2, where the
functions do not exist upstream.
`math.frexp` returns a binary fraction plus an exponent number in Luau and Lua
5.1–5.2 or an exponent integer in Blu and Lua 5.3–5.5. It preserves signed
zero and handles subnormal and non-finite values without an intermediate
overflow. `math.ldexp` composes the pair; Luau and Lua 5.1–5.2 truncate a
fractional exponent, while modern profiles require an integer-representable
exponent.
The legacy `math.sinh`, `math.cosh`, `math.tanh`, `math.log10`, and
`math.atan2` names exist in Blu, Luau, and Lua 5.1–5.4. Lua 5.5 removed them,
so its profile returns a structured unsupported-feature error instead of
silently substituting another function.
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
`math.map`. The arithmetic helpers return numbers, preserve Luau NaN behavior,
use ties-away-from-zero rounding, and reject inverted clamp bounds
structurally. `math.lerp` returns its second endpoint exactly when its factor is
one, preserving the pinned overflow-avoidance behavior. Lua profiles reject
these functions explicitly because they are absent from the corresponding
standard libraries.
`math.noise` ports the pinned Luau three-dimensional Perlin implementation,
including its `f32` intermediates, optional zero-valued coordinates, 256-unit
input wrapping, and deterministic outputs. Blu exposes the same contract; Lua
profiles reject the extension explicitly.
The `bit32` library exposes `band`, `bor`, `bxor`, `bnot`, `lshift`, `rshift`,
`arshift`, `lrotate`, `rrotate`, `extract`, and `replace` in Blu, Luau, Lua
5.2, and Lua 5.3 profiles. Luau truncates numeric inputs toward zero, Lua 5.2
rounds them ties-to-even, and Lua 5.3 requires integer-representable inputs;
strings use the active profile's numeric grammar. Results are numbers in Luau
and Lua 5.2 and integers in Blu and Lua 5.3. Lua 5.1, 5.4, and 5.5 reject
these functions explicitly because their standard libraries do not expose
`bit32`. Blu deliberately selects Luau's input conversion with Lua 5.3-style
integer results. Field offsets and widths are range-checked structurally.
`tonumber` preserves existing numeric subtypes and integer string conversions
for Blu and Lua 5.3–5.5, returns numbers for legacy profiles, accepts ordinary
hexadecimal integer and floating strings, and follows profile-specific explicit-base parsing,
non-finite spelling acceptance, and overflow behavior.
Integral counts and bytes returned by `rawlen`, `select("#", ...)`,
`string.len`, `string.byte`, and `table.pack.n` likewise use integers in Blu
and Lua 5.3–5.5 and numbers in legacy profiles. Array keys exposed by `next`
and the initial and advancing indices of `ipairs` use the same profile split.
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
advances by one byte after an empty match so iteration terminates.
`string.gsub` supports string, numeric, direct table, and function replacements.
Table replacement keys use the first
capture or the full match, position captures use the active profile's numeric
subtype, and nil or false values retain the original match. Function
replacements receive the captures, or the full match when there are no
captures. Synchronous table `__index` replacement handlers are supported. In
the owned BluV1 path, a callback may yield once per match: the callback frame,
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
and Lua-compatible empty-match progress. String and numeric replacements
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
`%%`, `%s` for string or numeric values, `%d`, `%i`, `%u`, `%x`, `%X`, `%o`,
`%c`, and default-precision `%f`, `%e`, and `%E`. Scientific exponents use an
explicit sign and at least two digits as required by the reference runtimes.
One- or two-digit field widths are supported for the implemented conversions,
including the shared `-` flag for left alignment. One- or two-digit explicit
precisions are supported for `%s`, `%f`, `%e`, and `%E`; `%.s` selects zero
precision as in the reference grammars.
Integer conversions truncate in Luau and Lua 5.1–5.2 and require an exact
integer representation in Blu and Lua 5.3–5.5. Output growth is fallible and
enforces the hard string limit. Other flags or flag combinations, field widths
wider than two digits, integer precisions, precisions wider than two digits,
`%g`/`%G`, `%a`/`%A`, `%q`, and non-scalar `%s` behavior remain structured
unsupported features rather than being approximated.
Blu and Luau provide `string.split` with a default comma separator,
non-overlapping byte-string separators, retained empty fields, and byte-wise
splitting for an empty separator. Its output table capacity is checked before
allocation. Lua profiles reject the Luau-only function explicitly.
Blu and Luau also provide bounded `table.create` and `table.find`.
`table.create` preflights its array capacity and optionally fills every slot;
`table.find` searches the contiguous array sequence from a positive optional
start and returns a profile-typed index. Lua profiles reject both Luau-only
functions explicitly.
`table.clear` removes array and hash entries without reallocating the table.
`table.clone` performs a bounded shallow copy, so self-references still point
to the source, and preserves unprotected metatables. Protected metatables
produce a structured error as in Luau. Both functions are Blu/Luau-only.
`table.freeze` marks a table shallowly immutable and `table.isfrozen` exposes
that state. Indexed writes, `rawset`, `table.clear`, sorting, and metatable
changes all enforce the same heap-level flag. Freezing twice and freezing a
protected-metatable table fail structurally; shallow clones are mutable.
Legacy `table.getn` is available in Blu, Luau, and Lua 5.1; `table.maxn` is
available in Blu, Luau, and Lua 5.1–5.2. Later Lua profiles reject these names
explicitly. Blu returns an exact integer from `getn`; `maxn` remains a number
because fractional numeric keys participate in its upstream contract.
The Lua 5.1 `table.foreach` and `table.foreachi` callbacks are available in
Lua 5.1 and Blu. They invoke callbacks with key/value or index/value pairs,
return the first non-nil callback result, and otherwise return nil. Owned
callbacks retain iteration state across yields, including terminal return
calls. In profiles that define it, `pairs` also invokes `__pairs`; owned
handlers have the same resumable operation boundary.
Legacy `gcinfo` is available in Blu, Luau, and Lua 5.1 and reports the
runtime's accounted live memory in whole KiB. Blu returns an integer; the
number-only compatibility profiles return a number. Lua 5.2–5.5 reject the
removed function explicitly.
`coroutine.running` follows the active profile: Lua 5.1 returns nil on the main
thread, Luau returns only the thread, and Blu/Lua 5.2–5.5 also return the
main-thread boolean. `coroutine.isyieldable` is true on Luau's main thread,
false on the Blu/Lua 5.3–5.5 main thread, true inside their coroutines, and
explicitly unsupported for Lua 5.1–5.2 where it is absent.
`collectgarbage` supports the shared `collect` and `count` commands. Collection
traces active frames, globals, threads, upvalues, and host-retained values.
`count` reports the runtime's accounted GC-heap kibibytes; it is not presented
as whole-process memory. Pinned Luau returns no values from `collect`; Lua
profiles return zero using their legacy-number or modern-integer policy. Other
commands differ by upstream version and fail explicitly until profile-dispatched.
`table.sort` supports bounded default ascending order for uniform numeric
sequences without NaN and uniform byte-string sequences. It returns no values
and accepts an omitted or nil comparator. Numeric sorting uses exact mixed
integer/number ordering without `f64` round-trip loss. Custom comparator
callbacks and `__lt` metamethod ordering use the bounded callback bridge;
owned callbacks retain insertion-sort state across yields, including GC roots
and terminal return calls.
`table.pack` and `table.unpack` are available in Blu, Luau, and Lua 5.2–5.5;
Lua 5.1 rejects those table-library names explicitly. The legacy global
`unpack` is available in Blu, Luau, and Lua 5.1 and is rejected in Lua
5.2–5.5, where the reference runtimes moved it into the table library.
`table.move` performs bounded overlap-safe moves and returns the destination
table for Blu, Luau, and Lua 5.3–5.5. Lua 5.1–5.2 calls fail with an explicit
unsupported-profile error because those libraries do not define it.

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
preload and host-loader searchers installed by default. Filesystem path,
native-library, and yielding searchers remain outside this slice. Portable
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
