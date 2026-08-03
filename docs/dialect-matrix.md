# Blu dialect and semantic profile matrix

Status: frontend gate 1 working contract

This document separates language reference behavior from the behavior currently
implemented by Blu. It is intentionally conservative: an unspecified Blu
choice is a blocker, not permission to inherit whichever behavior is easiest
to compile.

The frontend rules in
[ADR 0002](adr/0002-blu-owned-frontend.md) apply throughout:

- every source module and serialized prototype has an explicit profile;
- profile selection is never inferred from accepted syntax in a published
  artifact;
- profile-sensitive behavior is retained through HIR, MIR, bytecode, and VM
  frames; and
- an unsupported or unresolved feature produces a structured error rather than
  a silent fallback.

## Status vocabulary

| State | Meaning |
|---|---|
| `unsupported` | Blu rejects the profile or feature structurally, or the required semantics are absent. |
| `experimental` | Some source executes, but applicable reference suites and the deviation gate are incomplete. No compatibility claim is made. |
| `conformant` | The pinned reference corpus and applicable official suites pass, with all intentional deviations published. |

Status applies independently to each profile and domain. A working example or
a shared syntax subset is not conformance. No domain in this document is
currently `conformant`.

## Pinned reference identities

The machine-readable source of truth is
[`UPSTREAM.toml`](../UPSTREAM.toml). Checksums below are copied from that file,
not redefined by this document.

| Profile | Normative reference | Pin |
|---|---|---|
| `luau` | Luau source, runtime, parser, compiler, and conformance behavior under the default flags of the pinned build | Git revision `f8ca77acdcb50241e3da21af663f8ef97b4b5ce4` (upstream release 731) |
| `lua51` | Manual, interpreter, libraries, and tests for the official source release | Lua 5.1.5, SHA-256 `2640fc56a795f29d28ef15e13c34a47e223960b0240e8cb0a82d9b0738695333` |
| `lua52` | Same categories for Lua 5.2 | Lua 5.2.4, SHA-256 `b9e2e4aad6789b3b63a056d442f7b39f0ecfca3ae0f1fc0ae4e9614401b69f4b` |
| `lua53` | Same categories for Lua 5.3 | Lua 5.3.6, SHA-256 `fc5fd69bb8736323f026672b1b7235da613d7177e72558893a0bdcd320466d60` |
| `lua54` | Same categories for Lua 5.4 | Lua 5.4.8, SHA-256 `4f18ddae154e793e46eeab727c59ef1c0c0c2b744e7b94219710d76f530629ae` |
| `lua55` | Same categories for Lua 5.5 | Lua 5.5.0, SHA-256 `57ccc32bbbd005cab75bcc52444052535af691789dba2b9016d5c50640d68b3d` |
| `blu` | This matrix, the [language contract](language-contract.md), accepted deviation records, and subsequent accepted ADRs | Repository versioned contract; no upstream language owns this profile |

The bundled `luau0-src` crate is separately pinned at
`0.20.7+luau728`. It is the current bootstrap source compiler and a future
legacy oracle. It does not redefine the release-731 `luau` profile. Features
behind Luau fast flags are part of the profile only when enabled by the default
pinned oracle build or by a separately recorded profile configuration.

## Reference semantic matrix

Each row has a stable domain ID for tests and deviation records. “Oracle”
means the exact pinned runtime and its manual/tests control details not
restated here; it does not permit Blu to choose a different result.

### Syntax, numerics, and operators

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `LEX` | Lua 5.1-derived byte syntax plus typed Luau syntax, `continue`, compound assignment, if-expressions, generalized iteration, backtick interpolation, and other features accepted by the pinned default parser. Fast-flag-only syntax is excluded unless pinned as enabled. | 5.1 tokens, comments, quoted/long strings, decimal and hexadecimal numerals; no labels, attributes, `continue`, or type syntax. | Adds 5.2 lexical/grammar behavior including labels/`goto`, escapes, and hexadecimal floats. | 5.2 family plus `//` and bitwise tokens and 5.3 numeral grammar. | 5.3 family plus local attributes `<const>` and `<close>`. | 5.4 family plus `global` declarations and optional named vararg table syntax (`... name`). | The owned Blu frontend accepts the bounded typed Lua-family slice plus Blu/Luau `continue`, compound assignment, if-expressions, generalized iteration, and Luau backtick interpolation. Interpolation evaluates each embedded expression through `tostring`, supports nesting, and retains source spans; syntax outside this slice remains an explicit rejection. |
| `NUM` | Pinned Luau numeric behavior. Default-feature behavior is normative; experimental fast-flag integer syntax or libraries are not assumed. | One configured `lua_Number` domain; official build uses double. Arithmetic follows 5.1 coercion and modulo rules. | One number domain with 5.2 conversion and numeral rules. | Integer and float subtypes; default 64-bit integer/double build, wrapping integer arithmetic, `/` produces float, `//` floors. | 5.3 numeric model with 5.4 conversions and errors. | 5.4 numeric model with 5.5 conversions and errors. | Partially assigned. Fitting decimal integers are exact signed 64-bit values; larger decimals fall back to numbers. Hexadecimal and binary integer literals wrap through 64 bits. Integer addition, subtraction, and multiplication wrap; modulo and floor division use floor semantics. Mixed arithmetic promotes to numbers, and `/` and exponentiation return numbers. Numeric-string arithmetic follows Lua 5.4 integer-preserving conversion; covered math numeric APIs coerce numeric strings through the active profile parser, with modern min/max preserving the selected original operand. Sparse-table length consumers use the profile's compact or allocated legacy array boundary; `table.maxn` remains maximum-key based. Mixed integer/number comparisons are exact across `i64`; NaN is unordered. Modern bitwise operations and integer-aware library results are normative. Remaining conversion edge cases still require explicit decisions. |
| `OP` | Pinned operators include Luau compound assignments and `//`; logical `and`/`or` return operands. No Lua 5.3 bitwise syntax is inferred. | Arithmetic, comparison, concatenation, length, and short-circuit logical operators; no `//` or bitwise operators. | Same operator families as 5.1; no `//` or bitwise operators. | Adds `//`, `&`, `|`, binary/unary `~`, `<<`, and `>>` with integer conversion and corresponding metamethods. | 5.3 operator set and 5.4 precedence/coercion behavior. | 5.4 operator set and 5.5 precedence/coercion behavior. | Shared arithmetic, comparison, concatenation, length, and short-circuit operators are assigned. `//` uses Lua 5.3+ integer-preserving semantics; modern bitwise operators use 64-bit integer semantics. Their metamethods are resumable. Unassigned operators remain explicit rejections. |

### Bindings, evaluation, and calls

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `ENV` | Lua 5.1-style function environments and pinned `getfenv`/`setfenv` behavior; `_ENV` is not substituted for free names. | Globals resolve through function/thread environments; `getfenv` and `setfenv` are language-library mechanisms. | Free names are translated through lexical `_ENV`; `load` accepts an environment; 5.1 `getfenv`/`setfenv` are removed from the base library. | `_ENV` model as revised by 5.3. | `_ENV` model as revised by 5.4. | `_ENV` plus 5.5 lexical global declarations; undeclared free-name behavior follows the active global-declaration mode. | No environment model is yet selected. Host authority is orthogonal and cannot substitute for language environment semantics. |
| `ASSIGN` | Pinned Luau value-list adjustment, compound assignment, table-constructor, and overlapping-target order. | RHS values are evaluated/adjusted before assignment; constructor and overlapping-target observables follow the 5.1 oracle. | 5.2 oracle. | 5.3 oracle; the manual explicitly leaves constructor assignment order undefined. | 5.4 oracle; constructor assignment order remains undefined. | 5.5 oracle; constructor assignment order remains undefined. | Owned BluV1 preserves left-to-right target/key evaluation and RHS snapshotting, then commits adjusted destinations right-to-left as pinned across Lua 5.1–5.5/Luau. Constructor key/value evaluation remains source-ordered for the covered fields; broader constructor ordering remains intentionally unspecified. |
| `TAIL` | Proper tail recursion and pinned tail-call stack/debug behavior; the pinned corpus includes deep tail recursion. | Proper tail calls for a return whose expression is a single function call, subject to 5.1 grammar. | Proper tail-call guarantee under 5.2 rules. | Proper tail-call guarantee under 5.3 rules. | Proper tail calls except when the call remains inside the scope of a to-be-closed variable. | 5.4 rule as revised by 5.5. | Owned sole-call returns now replace the current frame and pass depth-2048 recursion across Luau and Lua 5.1–5.5; Lua 5.4–5.5 tail calls inside `<close>` scopes are de-optimized and pass cleanup ordering. Exact native-frame/debug metadata and broader close/error observables remain isolated. |

### Iteration and metamethods

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `ITER` | Numeric/generic `for`, `pairs`, `ipairs`, direct table iteration, and generalized iteration through pinned `__iter` behavior. | Generic-for iterator/state/control triplet; `pairs` uses `next`; `ipairs` walks positive integer keys until the first absent value. | Adds `__pairs` and `__ipairs` library hooks. | Keeps `__pairs` and `__ipairs`. | 5.3 iteration model under 5.4 library rules; `__ipairs` remains active. | 5.4 iteration model under 5.5 library rules; `__ipairs` is ignored. | Owned profiles dispatch `__ipairs` in Lua 5.2–5.3 with bounded resumable callbacks; Lua 5.1/5.4/5.5 retain raw `ipairs`. Blu/Luau owned direct-table loops now dispatch `__iter` once and otherwise use `next`; the pinned slice covers iterator mutation, a rooted yielding `__iter` callback, and the profile-specific `foreachi` snapshot. `table.foreach`/`foreachi` are available in Blu, Luau, and Lua 5.1. Hash-key visitation order remains unspecified. |
| `META` | Pinned Luau core metamethods plus `__iter`; lookup side, equality-handler requirements, yield boundaries, and error text follow the pinned oracle. | 5.1 arithmetic, indexing, assignment, call, concat, length, equality, and ordering events; 5.1 lookup/handler rules are normative. | Adds table `__len`, `__pairs`, `__ipairs`, and 5.2 event-selection behavior. | Adds `__idiv` and bitwise metamethods; retains `__pairs` and `__ipairs`. | Adds `__close` and 5.4 event-selection/finalization rules; `__ipairs` is no longer used. | 5.4 event set under 5.5 rules; `__ipairs` remains absent from dispatch. | No blanket inheritance. Core events, Lua 5.2–5.3 `__ipairs`, and Blu/Luau direct-table `__iter` dispatch are experimentally covered; owned `__iter` callbacks retain a rooted continuation across coroutine yields. Other events and conflicts must be selected or recorded as a deviation. |

### Coroutines, closing, and libraries

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `CORO` | Pinned create/resume/yield/wrap/status/running/isyieldable/close behavior. In the pinned CLI context `running` returns one value and the main execution thread is yieldable. Yieldability across protected calls and metamethod/C boundaries follows pinned tests. | Asymmetric coroutines; yields cannot cross protected-call or metamethod/C-call boundaries; `running` returns `nil` on the main thread. | Fully resumable coroutine model permits yields across Lua `pcall` and metamethods, subject to C continuation rules; `running` returns `(thread, is_main)`. | 5.2 model plus `coroutine.isyieldable`; the main thread is not yieldable. | 5.3 model plus `coroutine.close`, close-aware `wrap`, and the optional coroutine argument to `isyieldable`. | 5.4 coroutine model under 5.5 close/error rules. | Recorded choice: modern-Lua `running` pair and non-yieldable main thread. Other conflicts require differential evidence. |
| `CLOSE` | Upvalue closing, pinned userdata/finalizer behavior, and `coroutine.close`; no Lua 5.4 `<close>` syntax is inferred. | Upvalues close on scope exit; userdata `__gc`; no to-be-closed locals. | Adds table finalization under 5.2 marking/resurrection rules; no to-be-closed locals. | 5.2-style finalization under 5.3 rules. | Adds `<close>`, `__close`, reverse-order scope cleanup, close-on-control-transfer rules, and coroutine closing; `__gc` follows 5.4. | 5.4 model with 5.5 finalizer/close rules. | The owned slice now parses `<const>`/`<close>` and executes normal, reverse-order, `break`, return, `goto`, protected-error, and yielding `__close` paths. Full finalizer/GC and abandoned-coroutine semantics remain unresolved. |
| `LIB` | Pinned Luau library surface and return/error conventions, including Luau-specific libraries and sandbox omissions. | Official 5.1 base, coroutine, package, string, table, math, io, os, debug, and legacy `module`/`package.seeall` conventions. | Official 5.2 libraries, including legacy `module`/`package.seeall`, `bit32`, `table.pack`/`table.unpack`, `package.searchers`, and environment-aware loading. | Official 5.3 libraries, including integer-aware math and `utf8`; exact compatibility/deprecations follow 5.3. | Official 5.4 libraries, including warning and coroutine-close additions. | Official 5.5 libraries and revised global/vararg behavior. | System-capability libraries are a product goal, not current behavior. Recorded choices include modern separator-aware `string.rep`, deterministic ASCII-only byte case conversion, profile-gated core `utf8.len`/`utf8.codepoint`/`utf8.char`/`utf8.offset`/`utf8.codes` with `charpattern`, a pinned 7,999-result `table.unpack` ceiling for Blu/Luau, guest-table small-hash insertion order for Blu/Luau, and a separate `warn` channel for Blu and Lua 5.4–5.5; Lua 5.5 `utf8.offset` returns the additional final-byte position. All other conflicts need records. |

The `CLOSE`/`LIB` rows' remaining userdata gap is specifically guest-created
userdata allocation beyond Lua 5.1's `newproxy`. Lua 5.1–5.5 owned profiles now
cover non-forgeable host-created opaque userdata finalizers, including
profile-correct callback order, resurrection/rearming, and error/yield handling;
Lua 5.1 also covers `newproxy()`/`newproxy(true)` and shared metatables. A
trusted native library callback can create an opaque handle through
`Vm::create_userdata`; the Lua C stack, C-side `newuserdata`, and foreign C ABI
remain outside the owned bridge.

The profile inventory also differentially checks Lua 5.1's guest `newproxy`
allocation, including `newproxy(true)` metatable sharing and `__gc` execution;
the remaining guest-created allocation gap is the C-side `newuserdata`
authority, not this Lua 5.1 primitive.

The trusted native bridge also has a typed unavailable-result path: an embedding
can return Lua's standard `(nil, message, where)` shape with bounded `open`,
`absent`, or `init` status. Pinned builds have dynamic loading disabled, so the
conformance case compares the resulting shape and `absent` status against each
Lua 5.1–5.5 reference; real symbol binding and C continuation remain isolated.

The previously isolated discarded-before-first-use `io.lines()` cleanup is
now covered: the iterator is a heap-traced callable closure, and its opaque
file userdata is finalized in the same collection cycle as the pinned Lua
5.1–5.5 references. The same profile matrix now covers `io.tmpfile()` through
an explicit temporary-file host capability. File handles also expose
  `setvbuf` through a bounded host buffering capability, and `io.popen` is
  available only through an explicit host process/pipe capability.
The filename form of `io.lines` returns one value through Lua 5.3 and the
pinned four-value `(iterator, nil, nil, file)` shape in Lua 5.4–5.5; its
iterator raises host read failures, while direct file reads use `(nil, error)`.

## Minimum executable probes

Each domain must have positive, negative, and differential cases before its
state can advance. At minimum:

| ID | Required probes |
|---|---|
| `LEX` | Accepted/rejected token and grammar corpus per profile; malformed byte input; directive conflicts; exact byte spans. |
| `NUM` | Literal boundaries, signed zero, infinities/NaN where constructible, integer limits, overflow, coercion, division, modulo, and mixed-number comparisons. |
| `OP` | Precedence/associativity, short circuiting, divide-by-zero behavior, bit shifts, and metamethod fallback. |
| `ENV` | Nested closures, loaded chunks, environment replacement, `_ENV` shadowing, and 5.5 declaration modes. |
| `ASSIGN` | Overlapping lvalues, side-effecting indexes, multires adjustment, constructor duplicates, and unspecified-order non-claims. |
| `ITER` | Iterator triplets, holes, mutation, `pairs`, `ipairs`, direct tables, `__pairs`, `__ipairs`, `__iter`, errors, and yields. |
| `META` | Missing/shared/different handlers, operand selection, chained indexing, comparison fallback, errors, and yield boundaries. |
| `CORO` | Main/running results, nested resume/yield, protected calls, handlers that yield, close, errors after resumption, and C/native boundaries. |
| `TAIL` | Deep recursion, multires forwarding, debug stack visibility, varargs, yields, errors, and interaction with open upvalues/close scopes. |
| `CLOSE` | Normal exit, return, break, goto, error, tail call, reverse close order, replacement errors, resurrection, and abandoned coroutines. |
| `LIB` | Presence, signatures, multiple returns, coercions, byte behavior, error objects/text where required, locale effects, and authority-denied operations. |

Reference behavior is captured by running identical fixtures under the exact
pinned binary. Syntax-only cases use the pinned parser or `load`; runtime cases
compare returned values, output bytes, errors, stack behavior, and observable
library effects. Unspecified behavior in an upstream manual must not become a
Blu guarantee merely because one run produced a stable result.

## Current Blu implementation state

This table describes the repository as it exists now, not the intended
frontend.

| ID | `blu` state | `luau` state | `lua51`-`lua55` state | Current evidence and limitation |
|---|---|---|---|---|
| `LEX` | `experimental` | `experimental` | `experimental` | Public legacy execution still compiles source with pinned `luau0-src` release 728. The separate bounded `blu-syntax` frontend retains raw-byte spans and trivia, reconciles byte-zero dialect directives, and gates the initial token slice by profile. Its parser, resolver, and compiler cover semicolon separators/empty statements, local and assignment name/value lists, Lua 5.4/5.5 local attributes, optional bare or expression-list `return`, fixed-result postfix and colon-method calls plus call statements, fixed-width variadic function reads, dynamic vararg returns/final call arguments/final table fields, and final-field call MULTRET, lexical `do`/`end`, block-scoped `if`/`elseif`/`else`, `while`, `repeat`/`until`, numeric `for` with implicit unit or explicit nonzero literal steps, generic `for` in every owned profile, shared `break`, and Blu/Luau-only `continue`, nil/boolean/identifier expressions, the shared decimal integer/fraction/exponent subset, hexadecimal integers with explicit profile numeric representation, Blu/Luau numeric separators and binary integers, profile-gated hexadecimal numbers, quoted byte strings with shared decimal byte escapes and profile-gated hexadecimal byte escapes and whitespace-eating `\\z`, profile-gated Unicode escapes, shared long-bracket byte strings with explicit Lua/Luau newline differences, bounded table constructors with array/name/bracket fields and metamethod-aware bracket/dot reads, writes, and method lookup, grouping parentheses, unary `not`/`-` and byte-string/table `#`, profile-neutral `+`/`-`/`*`/`/`/`%`/`^`/`..`/`==`/`~=`/`<`/`<=`/`>`/`>=`/`and`/`or`, and profile-gated `//`. Exponentiation and concatenation are right-associative at their distinct shared precedences; comparisons, `and`, and `or` are left-associative in descending precedence below concatenation. Fixed scalar lists evaluate every RHS before binding/writing, discard extras, and fill missing values with `nil`; falling off the chunk emits an EOF-spanned zero-result return. Unresolved scalar identifiers use the VM global registry. |
| `NUM` | `experimental` | `experimental` | `experimental` | Basic number/integer VM values and arithmetic execute. BluV1 materializes integer constants for Blu and Lua 5.3–5.5 and explicitly rejects them for number-only profiles. The owned path emits exact fitting decimal integers and wrapping hexadecimal/binary integers for Blu, while fractions, exponents, and overflowing decimals remain numbers. Arithmetic parses trimmed decimal/hexadecimal strings with the pinned profile subtype split. Mixed numeric comparisons avoid lossy casts and cover bounds, fractions, infinities, and NaN. Blu and Lua 5.3–5.5 expose exact signed 64-bit `math.mininteger`/`math.maxinteger` bounds. Numeric `for` snapshots dynamic steps once; Blu and Lua 5.4–5.5 validate zero steps at runtime, while earlier profiles retain their assigned zero-direction behavior. Oversized explicit-base/default-hex conversions are now profile-differentially covered: Lua 5.1 saturates, Lua 5.2 and Luau accumulate as floats, and Lua 5.3–5.5/Blu wrap to 64-bit integers. Modern profiles reject non-integral `tonumber` bases while Lua 5.1/5.2 and Luau truncate them; `select` likewise truncates numeric selectors in Lua 5.1/5.2 and Luau but requires exact integer selectors in Blu and Lua 5.3–5.5. Whitespace/sign, decimal overflow, and hexadecimal-float grammar are covered across the pinned profiles. Other conversion edge cases remain incomplete. |
| `OP` | `experimental` | `experimental` | `experimental` | The owned path directly executes arithmetic, right-associative `..`, scalar equality/ordering, operand-returning short-circuit `and`/`or`, length, conditionals, and bounded loop control for all profiles. Floor division is enabled for Blu, Luau, and Lua 5.3–5.5 with explicit subtype behavior. Blu and Lua 5.3–5.5 execute 64-bit `&`, `\|`, binary/unary `~`, `<<`, and `>>`; all assigned operator metamethods resume through bounded continuations. Canonical instructions keep independent operands and validated control flow. Bootstrap translation rejects shapes it cannot preserve. Full numeric-boundary conformance and the broader owned expression set remain incomplete. |
| `ENV` | `experimental` | `experimental` | `experimental` | Owned Lua 5.1 supports string `loadstring`, function-targeted `getfenv`/`setfenv`, current-thread `setfenv(0, table)` rebinding, numeric `getfenv(1)`/`setfenv(1, table)` targeting a live closure frame with distinct thread/closure environments, and main-chunk environment rebinding with `__newindex`-aware global writes; arbitrary non-current stack levels and native-frame environments remain isolated. Blu and Luau now support the same bounded function/current-thread environment slice and environment-bound `loadstring` closures; their `loadstring` default follows the thread environment rather than the caller function's private binding. Owned Lua 5.2–5.5 has a rooted default chunk environment synchronized with the embedding registry, explicit lexical `local _ENV = table` reads/writes, nested closure capture, and environment-aware `load` returning persistent callable closures for an explicit or default environment. Owned `load` accepts bounded string-producing reader functions across Lua profiles, including resumable yielding readers, preserves supplied textual mode strings in rejection diagnostics, and accepts serialized BluV1 artifacts in binary mode. Arbitrary stack-level rebinding, foreign Lua binary chunks, and the complete foreign-binary mode matrix remain incomplete; yielding readers are an intentional owned extension because the pinned Lua binaries reject yields across `load`. Lua 5.5 named vararg tables and explicit `global`/`global *` declarations, scoped undeclared-name rejection, initializers, and closure propagation are implemented. Blu/Luau's unbound chunks still use the embedding global registry. |
| `ASSIGN` | `experimental` | `experimental` | `experimental` | Luau-generated bytecode covers basic tables, assignments, varargs, and multires. Owned BluV1 covers source-ordered constructor fields, metamethod-aware bracket/dot reads and writes, simultaneous mixed identifier/index/field assignment lists whose targets and right-hand sides are snapshotted before right-to-left commits, resumable final-field vararg/call MULTRET expansion, and nested final-call argument MULTRET forwarding. Broader assignment and constructor conformance remains incomplete. |
| `ITER` | `experimental` | `experimental` | `experimental` | Numeric and generic iterator loops execute in every owned profile with nil-only termination and profile-correct numeric-string controls; Lua 5.4/5.5 retain and close the fourth generic-for control on normal exit and `break`. Owned Blu/Luau direct-table iteration prepares the `__iter` hook or the `next` fallback, preserves callable function/table/userdata iterators, and reports non-iterable or non-callable hook results with profile-shaped errors. The pinned differential corpus reaches the official C++-installed `cYieldingIterator` host callback, which remains an explicit harness-capability isolation; the broader mutation/close matrix is absent. |
| `META` | `experimental` | `experimental` | `experimental` | Owned table reads/writes and method lookup follow table-valued `__index`/`__newindex` chains and resumably invoke closure/native handlers with pinned 100-step (Blu/Luau/Lua 5.1–5.2) or 2,000-step (Lua 5.3–5.5) bounds. Lua-family debug metatables for primitive nil/boolean/number/string/function/thread/lightuserdata types are VM-rooted and participate in scalar indexing, assignment, and operator lookup; Lua 5.2–5.5 and Blu also dispatch scalar `__len`, while Lua 5.1 retains its raw length behavior. Arithmetic, unary negation, concatenation, table length, comparison, callable-table, and Blu/Luau direct-table `__iter` handlers are covered; owned `__iter` callback preparation now retains its continuation across coroutine yields. Operator handler values may themselves use bounded `__call` chains. Comparisons require shared handlers in Luau/Lua 5.1–5.2 and use left/right lookup in Blu/Lua 5.3–5.5; Lua 5.5 alone omits reversed-`__lt` fallback. Other owned events, error text, and complete profile selection remain incomplete. |
| `CORO` | `experimental` | `experimental` | `unsupported` | Explicit legacy frames and owned BluV1 coroutine continuations support nested yields, protected calls, yielding handlers, resumable `load` readers, resumable guest package searchers, resumption errors, repeated owned closure resumes, and recorded Blu/Luau `running` differences. Owned BluV1 `coroutine.resume` now iteratively drives nested parent/child coroutine activations with rooted delivery, and the pinned Lua 5.1 portable child matrix passes all eight cases including `sieve.lua`. `xpcall` ignores trailing protected-function arguments only in Lua 5.1 and forwards them in Lua 5.2–5.5, Blu, and Luau. A handler that errors returns the profile-common `(false, "error in error handling")`; target numeric error values are stringified in Lua 5.1–5.2/Luau and preserved in Blu/Lua 5.3–5.5; Lua 5.5's `pcall`, `resume`, and `wrap` nil-error diagnostic is `<no error object>`. Dead-thread resume reports `cannot resume dead coroutine`; running-thread resume reports `cannot resume running coroutine` in Lua 5.1/Luau and `cannot resume non-suspended coroutine` in Lua 5.2–5.5/Blu. Post-yield table errors and close-failure objects preserve their values. `coroutine.close` succeeds for new/dead threads; running close raises in Luau/Blu and Lua 5.4, while Lua 5.5 returns zero values for a running thread and raises `cannot close main thread` for the main thread. `isyieldable(thread)` uses the main/non-main distinction in Blu and Lua 5.4–5.5; Lua 5.3/Luau ignore the optional target, and Luau reports non-yieldable execution inside native callbacks while direct guest frames remain yieldable. Invalid thread arguments use the pinned Luau/Blu `invalid argument` and Lua 5.4/5.5 `bad argument` forms; dead-close errors retain their string value. A yielding handler stays resumable, including a Lua 5.1 main-chunk `__newindex` handler as an owned extension; the pinned Lua 5.1 runtime rejects that metamethod-boundary yield. Owned BluV1 `error(message)`/level-1 source prefixes and level-0 raw messages are covered; deeper stack-level/source-frame diagnostics, native callback continuations, and arbitrary cross-thread continuation activation remain isolated. |
| `TAIL` | `experimental` | `experimental` | `experimental` | Owned sole-call returns replace the current closure frame and sustain deep recursion independently of the ordinary call-depth limit. Prefix returns retain a bounded continuation because they are not tail calls. Tail calls inside a to-be-closed scope are deliberately de-optimized; special native-frame metadata and complete close/error observables remain incomplete. |
| `CLOSE` | `experimental` | `experimental` | `experimental` | Owned Lua 5.4/5.5 parsing and execution covers `<const>`/`<close>` locals, fourth generic-for controls, const-write rejection, and `__close` on normal exit, `break`, return, `goto`, protected errors, generic-for body errors, repeat-condition cleanup timing, explicit `coroutine.close` cleanup of suspended values, reverse-order handler failure, and yielding handlers. Lua 5.2–5.5 table `__gc` callbacks now run once after explicit collection in reverse registration order and can resurrect their table; Lua 5.2–5.3 propagate the first finalizer error/yield, while Lua 5.4–5.5 continue after it. Lua 5.3–5.5 can explicitly re-arm a resurrected table from `__gc`, while Lua 5.2 remains one-shot. Conservative active-frame register liveness now closes the pinned Lua 5.3–5.5 re-arm cycle. Pinned Lua 5.4/5.5 no-implicit-close behavior for abandoned suspended coroutines is covered; guest userdata allocation/finalizers, full ordering beyond the covered registration case, and other abandoned-thread reclamation interactions remain incomplete. |
| `LIB` | `experimental` | `experimental` | `unsupported` | Initial base/string/table/math/coroutine functions and host `require` exist. Lua 5.1 additionally exposes `string.gfind` as the exact `string.gmatch` alias; later profiles omit that legacy name. Blu, Luau, and Lua 5.3–5.5 now expose the common `string.pack`/`unpack`/`packsize` binary-format core. `string.format` also covers shared integer precision, including sign/prefix handling and the precision-overrides-zero-width rule. `string.char` follows the profile numeric split: Lua 5.3–5.5/Blu require exact integer-representable arguments, Luau rejects non-finite values but truncates finite fractions, and Lua 5.1/5.2 retain truncating conversion. `string.byte` now applies the same modern exact-index rule, preserves Luau’s finite-truncation/NaN behavior, and keeps zero/out-of-range index boundaries consistent across profiles. `string.sub` and `string.find` apply the same modern exact-index versus legacy/Luau truncation split. The base `select` library now applies its differential-checked numeric-selector split: Lua 5.1/5.2 and Luau truncate numeric and numeric-string selectors, while Blu and Lua 5.3–5.5 require exact integer representation. Base `tostring` dispatches `__tostring`, requires a string result, and retains a yielding callback continuation inside owned Blu/Luau coroutines; pinned Lua profiles and Luau reject that yield across their native boundary. The owned runtime now also applies a differential-checked profile surface for presence-sensitive members (`type`, `rawget`, and iteration): Lua 5.5 has capacity-only `table.create`, Lua 5.2 retains global `unpack`, Lua 5.3–5.5 expose integer math helpers, Lua 5.4–5.5 expose `coroutine.close`, and removed/extension-only names are absent rather than call-time stubs. Lua-family owned profiles expose host-backed `package.loaded`/`package.preload` tables plus customizable profile-selected `package.searchers` (and Lua 5.1 `package.loaders`), profile-accurate `package.config`, PUC Unix/Windows default `package.path`/`package.cpath` strings, and a profile-correct `package.loadlib` boundary that returns the standard unavailable result until a native bridge is installed. Lua 5.1–5.5 also expose `os.clock`, no-argument `os.time`, callback-backed `os.date`, `os.difftime`, and capability-gated `os.getenv`; `os.clock`, `os.time`, and `os.date` require explicit host sources; Blu/Luau keep `os` hidden. Lua 5.1–5.5 additionally expose bounded opaque file handles through an explicit host opener: `io.open`/`io.type`/`io.close` plus optional single- or multi-format file `read`, optional host-authorized numeric `read` through `IoFile::read_number`, `write`/`seek`/`flush`, `file:lines()` line or numeric iteration with multiple formats per iteration rooted until EOF or explicit close, and an explicit stream provider for `io.stdin`/`io.stdout`/`io.stderr`, default or filename-backed `io.input()`/`io.output()`, default `io.read`/`io.write`/`io.flush`, and default-or-filename `io.lines()`; Blu/Luau keep `io` hidden. Discarded-before-first-use iterator cleanup remains isolated. The bounded debug metatable slice is exposed only in Lua 5.1–5.5; Luau keeps an empty `debug` table and Blu hides it. Host-backed IO userdata retain metatables through `getmetatable`/`debug.setmetatable`, `__index`, and `__newindex`; table finalizers are implemented for Lua 5.2–5.5, while guest userdata allocation/finalization remains incomplete. `require` dispatches through bounded preload and host-loader searchers, forwards a searcher’s extra loader value, supports resumable guest searchers, and caches results, while Blu/Luau retain their sandboxed host `require` without a guest `package` table. Owned Blu and Lua 5.1–5.5 profiles also expose capability-gated `loadfile`/`dofile` through an explicit host file-loader callback; Lua 5.2–5.5 expose `package.searchpath` through an explicit host path-probe callback, while Lua 5.1 and Luau hide it. With default or guest-configured `package.path` plus both explicit file capabilities, Lua 5.1–5.5 also support source-backed `require`, while an embedding module loader retains precedence. Native-library loading, yielding loader callbacks, exact profile libraries, broader debug/io APIs, system libraries, and full return/error conventions remain incomplete. |
| **Overall** | **`experimental`** | **`experimental`** | **`experimental`** | The owned baseline compiles and directly executes a deliberately small source slice for every explicit profile. The current conformance harness differentially checks 34 selected Luau fixtures: 25 reference passes per owned profile, with 12 Blu-profile semantic isolations, 0 Luau-profile semantic isolations, and 9 standalone-reference harness isolations. It also runs the pinned Lua 5.1 matrix (9/9), plus 16 selected Lua 5.4.8 and 5.5.0 cases: Lua 5.4 passes 13/16 with 3 precisely isolated output-only differences, and Lua 5.5 passes 12/16 with 4 precisely isolated output-only differences. The modern harness treats `math.lua` as assertion-checked because its random output is intentionally nondeterministic; the remaining isolated cases have passing executable assertions but differ in progress/call/syntax bytes or host-dependent C-stack counts. This selected official corpus therefore measures 34/41 (82.9%) under harness rules, not full language/library/ABI readiness. Lua binaries still provide a shared portable smoke matrix rather than full profile conformance. |

The owned Blu/Luau parser now erases simple function parameter and return type
annotations, local binding annotations, and anonymous vararg annotations
(including qualified names; Blu also accepts identifier union/intersection
forms and both profiles accept a simple optional `?` suffix) while retaining
the untyped AST. Balanced table, generic, and function-type containers are also
consumed for erasure; type aliases/declarations, nested generic `>>` token
forms, and Luau union/intersection token syntax remain explicit parser
boundaries. Luau/Blu expression type assertions of the form
`expression :: identifier` are also erased; Luau labels remain rejected even
though they share the `::` token.
Luau/Blu assignment statements may use a parenthesized prefix expression as
the root of an index or field target, including across a newline after a table
constructor; the owned compiler preserves the evaluated target before the
assignment write.

The profile-wide `LIB` surface also differentially covers Lua 5.1's exact
`string.gfind` alias and legacy `module`/`package.seeall`, Lua 5.2's legacy
module helpers without `string.gfind`, and omission of those names in Lua
5.3–5.5, Blu, and Luau. Lua 5.1 module calls rebind the owned caller
environment; Lua 5.2 returns the module table without that rebinding.

The profile-wide string-index audit also covers `string.match`, `string.gsub`
limits, and `string.rep` counts: modern profiles require exact
integer-representable arguments, while Lua 5.1–5.2 and Luau truncate finite
fractions. Lua 5.2 treats a NaN `string.gsub` limit as its default unlimited
bound, unlike Lua 5.1 and Luau. The optional `string.gmatch` start index is
accepted in Blu, Luau, and Lua 5.4–5.5; Lua 5.1–5.3 ignore an extra third
argument.

The table-index audit applies the same modern exact versus legacy/Luau
truncating conversion to `table.concat`, `table.unpack`, and `table.move`.
`table.create` truncates Luau counts but requires exact integer-representable
counts in Blu and Lua 5.5; unsupported profile members remain absent.
For Blu and Luau, `table.clear` retains an existing array allocation's later
length boundary, and `table.clone` carries that explicit preallocated boundary
through the shallow copy; hash-only tables retain the compact boundary, while
guest hash entries preserve small insertion/traversal order through clones.
`table.insert` and `table.remove` use the same positional-index split, while
Luau/Blu-only `table.find` truncates its optional start in Luau and requires an
exact index in Blu.
`table.unpack` rejects more than 7,999 result values in Blu/Luau, matching the
pinned Luau result-arity boundary; Lua 5.2–5.5 retain their larger native
result range.
For Blu and Luau, an integral `table.insert` position outside `1..#t+1` is a
raw keyed write; legacy Lua profiles retain their range error. Blu and Luau
treat non-finite explicit positions as a successful no-op.

`math.random` keeps the Lua 5.2 number-domain interval rule for fractional
bounds: one bound generates from `1` through the fractional upper interval,
and two bounds preserve the fractional lower endpoint. Lua 5.1 and Luau
truncate finite fractional bounds; Blu and Lua 5.3–5.5 require exact integer
arguments.
`math.ldexp` exponents use the corresponding Lua 5.1–5.2/Luau truncating versus
Blu/Lua 5.3–5.5 exact-integer split.
`math.min`/`math.max` preserve left-to-right NaN selection across all pinned
profiles; Blu and Lua 5.3–5.5 also preserve the selected integer/float
subtype, while Lua 5.1–5.2 and Luau expose the result in their single-number
domain.
The optional second argument to `math.atan` is likewise profile-sensitive:
Blu and Lua 5.3–5.5 use `atan2`, while Lua 5.1–5.2 and Luau retain the
single-argument result. For the modern integer domain,
`math.abs(math.mininteger)` preserves the wrapped minimum integer (the pinned
Lua behavior), while the number-only profiles use their ordinary number result
path.
`math.fmod` keeps integer results and rejects an integer zero divisor in Blu
and Lua 5.3–5.5; Lua 5.1–5.2 and Luau retain floating-point NaN behavior for
that case. Non-finite operands remain NaN across the pinned profiles.

The binary cursor audit applies exact integer conversion to `string.unpack`
positions and integer fields in `string.pack` in Blu and Lua 5.3–5.5, while
Luau truncates finite fractional positions and integer-field values. The UTF-8
index audit applies exact integer conversion to `utf8.len`,
`utf8.codepoint`, `utf8.offset`, and `utf8.char` in Blu and Lua 5.3–5.5;
Luau truncates finite fractions, and Lua 5.1–5.2 keep the library absent.
Lua 5.4–5.5 additionally expose lax UTF-8 decoding for `len`, `codepoint`,
and `codes`, including legacy five- and six-byte forms through `0x7fffffff`;
Blu/Lua 5.3 retain their always-surrogate-tolerant profile and Luau stays
strict.

The remaining integer-bearing `os.date` timestamp and `debug.traceback` level
arguments use the modern exact versus Lua 5.1–5.2/Luau truncating split; Blu
keeps the host-authorized `os` and debug surfaces hidden, while pinned Luau
retains its corresponding standard functions.
Debug local and upvalue indices use the same Lua 5.1–5.2 truncating versus
Lua 5.3–5.5 exact conversion split. Lua 5.1 still omits `debug.upvaluejoin`
and `debug.upvalueid`; Lua 5.2–5.5 retain those APIs with their pinned
availability and bounds.
The `debug.sethook` count follows the same Lua 5.1–5.2 truncating versus Lua
5.3–5.5 exact conversion split; hook masks, clearing, and callback availability
remain profile-specific.
Numeric `debug.getinfo` stack levels follow that same Lua 5.1–5.2 truncating
versus Lua 5.3–5.5 exact conversion split; function and thread targets retain
their existing profile-specific behavior.
For `debug.getuservalue`, Lua 5.2–5.3 ignore the legacy extra index argument,
while Lua 5.4–5.5 validate it as an exact integer; the owned implementation
and differential probe preserve that distinction. `setuservalue` target and
uservalue restrictions remain profile-specific.

The profile-wide library probes also pin `collectgarbage("stop")` and
`collectgarbage("restart")` to Lua 5.1–5.5, `collectgarbage("isrunning")` to
Lua 5.2–5.5, and explicit collection while automatic collection is stopped;
Blu and Luau retain only the shared `collect`/`count` controls.

The owned `os.date("*t")` and `os.date("!*t")` forms are capability-backed
through `CalendarDate`, and `os.time(table)` forwards validated fields through
`CalendarDateInput`; calendar-table numeric fields use the modern exact versus
Lua 5.1–5.2/Luau truncating integer conversion split, while the host supplies
timezone/DST policy and Lua's `wday`, `yday`, and `isdst` values.

Lua 5.1–5.5 `os.remove` and `os.rename` are now explicit host-capability
boundaries with differential success- and failure-path coverage; callback
failures return Lua's `(nil, error)` shape while unavailable capabilities
remain structured host-policy errors.
The same recoverable failure shaping applies to configured `io.tmpfile` and
`io.popen` constructors; invalid constructor arguments remain raised
argument-boundary errors.
The opaque `io.seek` offset follows the Lua 5.1 truncating versus Lua 5.2–5.5
exact integer conversion split.
The optional `file:setvbuf` size follows the Lua 5.1–5.2 truncating versus Lua
5.3–5.5 exact integer conversion split.
Numeric `file:read(n)` byte counts follow the same Lua 5.1–5.2 truncating
versus Lua 5.3–5.5 exact integer conversion split.
Configured host failures from file read/write/seek/flush/setvbuf and explicit
close now return the recoverable `(nil, error)` shape, and the file handle is
marked closed before the host close callback so a failed close cannot be
retried; `io.lines` iterator read failures are raised like the PUC iterator
boundary; absent optional operation capabilities remain structured host-policy
errors.
Filename failures in `io.input` and `io.output` are raised, unlike the
recoverable `io.open` constructor result; closed file handles are rejected by
both stream setters before they replace the current stream.
Rebinding leaves the previous handle usable until explicit close or collection,
and argument-less `io.close()` closes the newly selected current output.
`os.execute` now has an explicit host process callback with profile-correct
Lua 5.1 numeric and Lua 5.2–5.5 tuple result mapping; the VM never launches a
process implicitly. `os.exit` likewise forwards a typed status/close request
to a host termination callback and never exits the embedding process itself.
`os.setlocale` and `os.tmpname` likewise require explicit host callbacks for
locale policy and name generation.

Lua 5.1–5.5 remain unsupported on the legacy Luau-bytecode `Engine::execute`
path and return a structured not-implemented error there. The profile-aware
`Engine::execute_owned_source` entry point now makes the explicit owned
compiler plus direct BluV1 baseline available as a testable public path. Their
experimental status above still refers only to that bounded slice; it is not a
broader compatibility claim.

## Deviation ledger

The owned frontend now lexes and lowers Lua-style `::label::` declarations
and `goto` for Blu and Lua 5.2–5.5. These are validated as ordinary forward
or backward branches, with same-lexical-scope targets only; cross-scope jumps
remain rejected until upvalue-closing control flow is available. Luau and Lua
5.1 reject the syntax during lexing.

Owned `string.gsub` also has an explicit resumable operation continuation for
owned callbacks: each match can yield and later receive its replacement value
without restarting the pattern scan. A replacement-table `__index` callback
that yields remains rejected at the native boundary, matching the pinned
Luau/Lua 5.1–5.5 references. Other yielding library callbacks remain
unresolved.

Owned `table.sort` preserves insertion-sort state across yielding custom
comparators and `__lt` handlers, and profile-available `pairs` invokes
`__pairs` through a resumable operation boundary. `table.foreach` and
`table.foreachi` are available in Blu, Luau, and Lua 5.1; `foreachi` follows
the profile-specific compact versus allocated legacy length boundary. Native and legacy callback
families outside these explicitly covered operations remain unresolved.

An intentional difference from a reference profile requires a versioned entry.
Unresolved behavior is not a deviation; it remains unsupported.

### Long-tail isolation ledger

The remaining compatibility gaps are deliberately separated from the owned
execution contract:

- **Foreign Lua binary chunks:** pinned `luac` probes produce distinct Lua
  5.1–5.5 headers and version-dependent instruction/constant encodings. Blu
  accepts only its validated BluV1 binary envelope. An executable Lua 5.1
  probe generates a pinned `ESC Lua Q` chunk, confirms Lua 5.1 loads it, and
  confirms Blu rejects it as a foreign binary. A decoder remains isolated
  because accepting it would require translating version/ABI-dependent
  instructions and constants into BluV1 while preserving profile authority
  and resource limits. The conformance runner now repeats the rejection probe
  for pinned Lua 5.1–5.5 chunks (headers `Q` through `U`), with each native
  reference loading its own chunk before Blu rejects it.
- **Full native ABI:** the trusted bridge can return native callbacks and
  opaque host-owned userdata, including typed `package.loadlib` failure
  results. It does not provide `lua_State`, a Lua C stack, a raw allocator,
  C-side `newuserdata`, foreign binary loading, or ABI-compatible C-module
  continuations. A yielding native loader is executable-tested and reports
  `CoroutineYieldOutside`.
- **Broad yielding native callbacks:** owned continuations exist for the
  explicitly listed reader, package-searcher, package-loader, metamethod, comparator, and
  selected library operations. Other native/library callbacks remain
  operation-specific unsupported boundaries rather than silently yielding.
- **Package-loader continuation:** owned Lua 5.1–5.5 profiles retain the
  selected loader, module name, and `package.loaded` roots across a loader
  yield; pinned Lua 5.1–5.5 reject that yield at the native `require`
  boundary. Blu and Luau keep `package` hidden, so this extension does not
  change their sandboxed module surface.
- **Long-tail numeric/library edges:** the current differential corpus covers
  the profile-presence matrix, integer bounds, conversion boundaries including
  oversized base/hex overflow rules, base validation, and shared number
  grammar, binary
  formats, and the implemented capability libraries. Full official-suite
  return/error wording, locale-dependent behavior, filesystem/process failure
  shaping, collector tuning/mode commands that require an incremental or
  generational scheduler, and the unimplemented library families remain
  follow-up work. The collector command boundary is executable across Lua
  5.1–5.5, including the profile-specific presence and return-type split.

### Required schema

| Field | Requirement |
|---|---|
| `id` | Stable identifier, for example `DEV-BLU-LIB-001`. Never reuse a retired ID. |
| `status` | `proposed`, `accepted`, `temporary`, `superseded`, or `removed`. Only `accepted` entries define the Blu profile. |
| `profile` | Profile whose behavior differs; normally `blu`. Compatibility profiles require exceptional justification. |
| `domain` | One matrix ID from this document. |
| `reference` | Exact profile/version/revision, feature flags, manual/test anchor, and oracle command. |
| `reference_behavior` | Observable reference result, including values, errors, output, stack, yield, or library effects. |
| `blu_behavior` | Exact proposed/implemented Blu result. “Similar” is insufficient. |
| `classification` | `extension`, `intentional divergence`, or `temporary bootstrap`. |
| `rationale` | User-visible reason; implementation convenience alone is insufficient. |
| `artifact_impact` | Required profile metadata, opcode/IR behavior, serialization, and compatibility consequences. |
| `tests` | Positive, negative, differential, cross-profile, and migration fixtures. |
| `documentation` | User-facing contract location and release note. |
| `owner_and_date` | Responsible owner, decision date, and reviewing ADR or issue. |
| `removal_or_migration` | How temporary behavior is rejected, migrated, or retired without silent drift. |

Ledger rules:

1. No deviation can weaken ADR 0001's validated-artifact, authority, resource,
   or isolation gates.
2. No entry authorizes compiler fallback, profile guessing, or relabeling a
   Luau artifact as canonical Blu bytecode.
3. Compatibility profiles (`luau`, `lua51`-`lua55`) remain exact by default.
   A known incompatibility keeps the affected domain experimental and must be
   published; it is not silently normalized.
4. Where the reference deliberately leaves behavior undefined, Blu may define
   behavior only through an accepted Blu entry. Otherwise the implementation
   and tests must avoid promising a result.
5. Exact error text is compared only when the profile contract or accepted
   entry makes it observable; structured category, value, and source location
   are always retained.

### Decisions already recorded by the language contract

These are the only current Blu-specific decisions in the covered conflict
areas. Their implementation remains `experimental`.

| ID | Domain | Reference behavior | Blu behavior | Status |
|---|---|---|---|---|
| `DEV-BLU-LIB-001` | `LIB` | Pinned Luau ignores the optional separator argument to `string.rep`. | Blu accepts the modern Lua separator form. | `accepted` by the current language contract; conformance evidence incomplete |
| `DEV-BLU-LIB-002` | `LIB` | Locale and non-ASCII case behavior differs among references and hosts. | `string.lower` and `string.upper` map ASCII bytes deterministically and leave every other byte unchanged. | `accepted` by the current language contract; conformance evidence incomplete |
| `DEV-BLU-CORO-001` | `CORO` | Pinned Luau `coroutine.running` returns one value and its main execution context is yieldable. | Blu returns `(thread, is_main)` and makes the main thread non-yieldable, following modern Lua. | `accepted` by the current language contract; conformance evidence incomplete |
| `DEV-BLU-ITER-001` | `ITER` | Pinned Luau rejects a yield crossing the native `__iter` preparation boundary. | Blu accepts a yielding `__iter` callback inside a coroutine and resumes the pending generic-for preparation with the returned iterator triplet. | `accepted` extension; differential boundary covered |
| `DEV-BLU-LIB-003` | `LIB` | Pinned Lua 5.1–5.5 and Luau reject a yield crossing the native `__tostring` boundary. | Blu accepts a yielding `__tostring` callback inside a coroutine and resumes with its first string result; owned Luau exposes the same bounded extension. | `accepted` extension; differential boundary covered |
| `DEV-LUA51-CORO-001` | `CORO` | Pinned Lua 5.1 rejects a yield crossing a main-chunk `__newindex` metamethod boundary. | The owned Lua 5.1 profile retains the guest continuation and resumes the handler after the yield. | `accepted` owned-profile extension; differential boundary covered |

No other unresolved choice in the reference matrix is implicitly accepted.

## Gate completion

Frontend gate 1 is complete only when:

1. every `blu` cell marked unresolved has either an accepted rule or a
   structured-rejection requirement;
2. every matrix row has pinned positive, negative, and differential fixtures;
3. official Lua suites are separately pinned and hashed, and applicable Luau
   suites run under the recorded default flag set;
4. the deviation ledger is published and test-linked; and
5. no profile is promoted beyond the implementation evidence in this document.

Until those conditions hold, this document is an executable backlog and an
anti-ambiguity contract, not a claim that the future frontend profiles are
complete.
