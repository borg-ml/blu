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
| `LEX` | Lua 5.1-derived byte syntax plus typed Luau syntax, `continue`, compound assignment, if-expressions, generalized iteration, backtick interpolation, and other features accepted by the pinned default parser. Fast-flag-only syntax is excluded unless pinned as enabled. | 5.1 tokens, comments, quoted/long strings, decimal and hexadecimal numerals; no labels, attributes, `continue`, or type syntax. | Adds 5.2 lexical/grammar behavior including labels/`goto`, escapes, and hexadecimal floats. | 5.2 family plus `//` and bitwise tokens and 5.3 numeral grammar. | 5.3 family plus local attributes `<const>` and `<close>`. | 5.4 family plus `global` declarations and optional named vararg table syntax (`... name`). | Typed Lua-family syntax is intended, but its exact accepted union is not yet frozen. The owned frontend must reject syntax not listed by a later accepted matrix revision. |
| `NUM` | Pinned Luau numeric behavior. Default-feature behavior is normative; experimental fast-flag integer syntax or libraries are not assumed. | One configured `lua_Number` domain; official build uses double. Arithmetic follows 5.1 coercion and modulo rules. | One number domain with 5.2 conversion and numeral rules. | Integer and float subtypes; default 64-bit integer/double build, wrapping integer arithmetic, `/` produces float, `//` floors. | 5.3 numeric model with 5.4 conversions and errors. | 5.4 numeric model with 5.5 conversions and errors. | Unresolved beyond behavior already exercised by the bootstrap. Blu integer, overflow, coercion, and mixed-number rules require an accepted decision before new syntax is enabled. |
| `OP` | Pinned operators include Luau compound assignments and `//`; logical `and`/`or` return operands. No Lua 5.3 bitwise syntax is inferred. | Arithmetic, comparison, concatenation, length, and short-circuit logical operators; no `//` or bitwise operators. | Same operator families as 5.1; no `//` or bitwise operators. | Adds `//`, `&`, `|`, binary/unary `~`, `<<`, and `>>` with integer conversion and corresponding metamethods. | 5.3 operator set and 5.4 precedence/coercion behavior. | 5.4 operator set and 5.5 precedence/coercion behavior. | Modern operators are intended, but each operator's numeric and metamethod behavior must be assigned explicitly. Unassigned operators are rejected. |

### Bindings, evaluation, and calls

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `ENV` | Lua 5.1-style function environments and pinned `getfenv`/`setfenv` behavior; `_ENV` is not substituted for free names. | Globals resolve through function/thread environments; `getfenv` and `setfenv` are language-library mechanisms. | Free names are translated through lexical `_ENV`; `load` accepts an environment; 5.1 `getfenv`/`setfenv` are removed from the base library. | `_ENV` model as revised by 5.3. | `_ENV` model as revised by 5.4. | `_ENV` plus 5.5 lexical global declarations; undeclared free-name behavior follows the active global-declaration mode. | No environment model is yet selected. Host authority is orthogonal and cannot substitute for language environment semantics. |
| `ASSIGN` | Pinned Luau value-list adjustment, compound assignment, table-constructor, and overlapping-target order. | RHS values are evaluated/adjusted before assignment; constructor and overlapping-target observables follow the 5.1 oracle. | 5.2 oracle. | 5.3 oracle; the manual explicitly leaves constructor assignment order undefined. | 5.4 oracle; constructor assignment order remains undefined. | 5.5 oracle; constructor assignment order remains undefined. | Unresolved. HIR must preserve source evaluation order and must not promise an order that the selected reference leaves undefined. |
| `TAIL` | Proper tail recursion and pinned tail-call stack/debug behavior; the pinned corpus includes deep tail recursion. | Proper tail calls for a return whose expression is a single function call, subject to 5.1 grammar. | Proper tail-call guarantee under 5.2 rules. | Proper tail-call guarantee under 5.3 rules. | Proper tail calls except when the call remains inside the scope of a to-be-closed variable. | 5.4 rule as revised by 5.5. | Proper tail-call behavior is required by the language contract, but exact Blu cleanup and debug behavior is unresolved. |

### Iteration and metamethods

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `ITER` | Numeric/generic `for`, `pairs`, `ipairs`, direct table iteration, and generalized iteration through pinned `__iter` behavior. | Generic-for iterator/state/control triplet; `pairs` uses `next`; `ipairs` walks positive integer keys until the first absent value. | Adds `__pairs` and `__ipairs` library hooks. | Keeps `__pairs`; `__ipairs` is no longer used by `ipairs`. | 5.3 iteration model under 5.4 library rules. | 5.4 iteration model under 5.5 library rules. | Direct table iteration is intended, but the final relationship among `__iter`, `__pairs`, and versioned `ipairs` semantics is unresolved. |
| `META` | Pinned Luau core metamethods plus `__iter`; lookup side, equality-handler requirements, yield boundaries, and error text follow the pinned oracle. | 5.1 arithmetic, indexing, assignment, call, concat, length, equality, and ordering events; 5.1 lookup/handler rules are normative. | Adds table `__len`, `__pairs`, `__ipairs`, and 5.2 event-selection behavior. | Adds `__idiv` and bitwise metamethods; retains `__pairs`; drops `__ipairs` use. | Adds `__close` and 5.4 event-selection/finalization rules. | 5.4 event set under 5.5 rules. | No blanket inheritance. Existing core events are experimental; each additional event and conflict must be selected or recorded as a deviation. |

### Coroutines, closing, and libraries

| ID | `luau` | `lua51` | `lua52` | `lua53` | `lua54` | `lua55` | `blu` |
|---|---|---|---|---|---|---|---|
| `CORO` | Pinned create/resume/yield/wrap/status/running/isyieldable/close behavior. In the pinned CLI context `running` returns one value and the main execution thread is yieldable. Yieldability across protected calls and metamethod/C boundaries follows pinned tests. | Asymmetric coroutines; yields cannot cross protected-call or metamethod/C-call boundaries; `running` returns `nil` on the main thread. | Fully resumable coroutine model permits yields across Lua `pcall` and metamethods, subject to C continuation rules; `running` returns `(thread, is_main)`. | 5.2 model plus `coroutine.isyieldable`; the main thread is not yieldable. | 5.3 model plus `coroutine.close`, close-aware `wrap`, and the optional coroutine argument to `isyieldable`. | 5.4 coroutine model under 5.5 close/error rules. | Recorded choice: modern-Lua `running` pair and non-yieldable main thread. Other conflicts require differential evidence. |
| `CLOSE` | Upvalue closing, pinned userdata/finalizer behavior, and `coroutine.close`; no Lua 5.4 `<close>` syntax is inferred. | Upvalues close on scope exit; userdata `__gc`; no to-be-closed locals. | Adds table finalization under 5.2 marking/resurrection rules; no to-be-closed locals. | 5.2-style finalization under 5.3 rules. | Adds `<close>`, `__close`, reverse-order scope cleanup, close-on-control-transfer rules, and coroutine closing; `__gc` follows 5.4. | 5.4 model with 5.5 finalizer/close rules. | Unresolved. Coroutine lifecycle support does not imply `<close>`, `__close`, or any finalizer guarantee. |
| `LIB` | Pinned Luau library surface and return/error conventions, including Luau-specific libraries and sandbox omissions. | Official 5.1 base, coroutine, package, string, table, math, io, os, debug, and module conventions. | Official 5.2 libraries, including `bit32`, `table.pack`/`table.unpack`, `package.searchers`, and environment-aware loading. | Official 5.3 libraries, including integer-aware math and `utf8`; exact compatibility/deprecations follow 5.3. | Official 5.4 libraries, including warning and coroutine-close additions. | Official 5.5 libraries and revised global/vararg behavior. | System-capability libraries are a product goal, not current behavior. Recorded choices include modern separator-aware `string.rep` and deterministic ASCII-only byte case conversion; all other conflicts need records. |

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
| `LEX` | `experimental` | `experimental` | `experimental` | Public legacy execution still compiles source with pinned `luau0-src` release 728. The separate bounded `blu-syntax` frontend retains raw-byte spans and trivia, reconciles byte-zero dialect directives, and gates the initial token slice by profile. Its parser, resolver, and compiler cover `local name = expression`, bare or expression-list `return`, nil/boolean/decimal-integer/identifier expressions, escape-free quoted byte strings, grouping parentheses, unary `not`, profile-neutral `+`/`-`, and profile-gated `//`. Profile-specific string escapes remain explicitly rejected. |
| `NUM` | `experimental` | `experimental` | `experimental` | Basic number/integer VM values and arithmetic execute. The owned path uses profile-specific literal storage and directly executes its baseline for all seven profiles; full pinned numeric boundaries and coercions are not gated. |
| `OP` | `experimental` | `experimental` | `experimental` | Arithmetic, logical, comparison, length, concat, and Luau `//` compatibility-bytecode paths exist. The owned path directly executes `+` and `-` for all profiles and `//` for Luau and Lua 5.3–5.5. Lua 5.3 bitwise syntax/semantics and the broader owned operator set do not. |
| `ENV` | `unsupported` | `unsupported` | `unsupported` | Globals exist, but complete `getfenv`/`setfenv`, `_ENV`, loaded-chunk environment, and 5.5 declaration semantics do not. |
| `ASSIGN` | `experimental` | `experimental` | `unsupported` | Luau-generated bytecode covers basic tables, assignments, varargs, and multires; profile-specific ordering suites are absent. |
| `ITER` | `experimental` | `experimental` | `unsupported` | Numeric/generic loops, `next`, `pairs`, `ipairs`, and direct table iteration have focused tests; the full `__iter` and versioned hook matrix is absent. |
| `META` | `experimental` | `experimental` | `unsupported` | An initial core metamethod subset is differentially exercised against Luau; profile selection, all events, and boundary behavior are incomplete. |
| `CORO` | `experimental` | `experimental` | `unsupported` | Explicit frames support nested yields, protected calls, yielding handlers, resumption errors, and recorded Blu/Luau `running` differences; official suites are incomplete. |
| `TAIL` | `unsupported` | `unsupported` | `unsupported` | Calls use a bounded explicit frame stack, but no supported proper-tail-call contract is implemented. |
| `CLOSE` | `unsupported` | `unsupported` | `unsupported` | Upvalue closing and `coroutine.close` exist; to-be-closed variables, `__close`, and complete finalizer semantics do not. |
| `LIB` | `experimental` | `experimental` | `unsupported` | Initial base/string/table/math/coroutine functions and host `require` exist. Exact profile libraries, system libraries, and full return/error conventions do not. |
| **Overall** | **`experimental`** | **`experimental`** | **`experimental`** | The owned baseline compiles and directly executes a deliberately small source slice for every explicit profile. The current conformance harness differentially checks a focused Luau corpus; Lua binaries still provide only a shared portable smoke matrix rather than profile conformance. |

Lua 5.1–5.5 remain unsupported on the legacy Luau-bytecode `Engine::execute`
path and return a structured not-implemented error there. Their experimental
status above refers only to the explicit owned compiler plus direct BluV1
baseline path; it is not a broader compatibility claim.

## Deviation ledger

An intentional difference from a reference profile requires a versioned entry.
Unresolved behavior is not a deviation; it remains unsupported.

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
