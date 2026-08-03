# ADR 0002: Own the Blu source frontend

- Status: Draft
- Date: 2026-07-29

## Context

Blu supports explicit `blu`, `luau`, and Lua 5.1 through 5.5 profiles. These
profiles have conflicting observable behavior in parsing, numbers,
environments, tail calls, metamethods, iteration, assignment ordering,
coroutines, finalization, errors, and standard libraries. A source frontend
therefore cannot infer a profile from accepted syntax or lower every profile
into one unannotated program representation.

The current source path is a bootstrap:

- `blu-compiler` calls the bundled `luau0-src` compiler through C FFI;
- `Engine` checks an optional dialect directive but supplies no profile to the
  compiler;
- `blu-bytecode` decodes and validates serialized Luau bytecode;
- the VM holds one global dialect, while chunks, prototypes, closures, and
  suspended frames do not retain a semantic profile; and
- package V1 can execute only Luau-format payloads for the `blu` and `luau`
  dialect tags.

This is useful for bringing up the VM and checking it against pinned Luau, but
it cannot implement the published per-prototype and cross-dialect semantics.
It also makes a foreign compiler part of the production path for all source,
including source that is not defined by Luau.

[ADR 0001](0001-blu-portable-component-runtime.md) requires complete validation
before execution and an opaque validated-artifact boundary. It also requires
that embedded and worker placement preserve package semantics and that no
isolation level silently weakens validation, authority, resource, or
portability guarantees. The source frontend must produce artifacts that meet
those gates; successful parsing or compilation is not authority to bypass
them.

## Decision

Blu will own a byte-oriented source frontend and a separately versioned Blu
bytecode format. The production pipeline will be:

```text
source bytes + explicit profile
  -> profile-aware lexer and parser
  -> resolved HIR
  -> control-flow MIR
  -> Blu register bytecode
  -> profile-aware validation
  -> execution under the callee prototype's profile
```

Published source and every serialized function prototype have an explicit
semantic profile. An embedding option, package manifest, CLI option, or initial
source directive may select it. These inputs must agree. Import tooling may
offer detection, but publication locks the result; compiler and runtime code
must not guess a profile from syntax.

### Crate and representation boundaries

- `blu-core` owns the canonical dialect/profile types, source identities,
  half-open byte spans, source storage, structured diagnostics, and compiler
  identity. It has no compiler or runtime dependency.
- `blu-syntax` owns byte lexing, directives, profile grammar gates, recovery
  parsing, retained trivia, and the spanned AST.
- `blu-ir` owns resolved HIR and control-flow MIR. HIR preserves binding
  identity, value-list adjustment, evaluation order, tail position,
  environments, and cleanup scopes. MIR makes control flow, temporaries,
  calls, close actions, and profile-sensitive operations explicit.
- `blu-compiler` orchestrates resolution, lowering, register allocation, and
  code generation. These passes remain in this crate until another consumer
  justifies a narrower crate.
- `blu-bytecode` owns the Blu format, encoder, decoder, source maps, opaque
  validated artifact, and profile-aware validator. Its existing Luau decoder
  remains a compatibility module.
- `blu-runtime` executes validated normalized artifacts. Each frame, closure,
  continuation, and prototype retains the callee profile. Native calls receive
  an execution context containing the active caller profile instead of
  consulting one VM-global dialect.
- `blu-lang` remains the public embedding facade and reconciles configured,
  manifest, and directive profiles through the frontend APIs.

HIR and MIR must not erase behavior that later passes cannot reconstruct.
Profile-sensitive operations include numeric modes, integer and bitwise
arithmetic, `_ENV` and legacy environments, proper tail calls, metamethod
dispatch, multireturn and vararg adjustment, table and assignment ordering,
iteration, and close/finalizer edges. Such operations must not be lowered to
ordinary library calls when that would change yields, stack traces, errors,
metamethods, or evaluation order.

### Blu bytecode

`BytecodeFormat::BluV1` and its backward-readable `BytecodeFormat::BluV2`
will be distinct from serialized Luau bytecode, even
where register layouts or individual operations are similar. Every prototype
will include:

- its semantic profile;
- register, parameter, vararg, constant, child, and upvalue metadata;
- code and required bytecode feature bits;
- source identity and a validated PC-to-byte-span map;
- function definition line ranges in BluV2; and
- local and upvalue debug ranges.

The normalized instruction set will add operations required by the supported
profiles, including integer and bitwise operations, tail calls, environment
access, and close-scope behavior.

Validation covers structural safety and profile legality: registers,
constants, jumps, initialized-value flow, cleanup edges, source-map
references, profile/opcode compatibility, and artifact resource limits.
Mutation invalidates validation. VM entry points continue to require the opaque
validated type required by ADR 0001 in both embedded and isolated placement.

### Why Luau bytecode is not the canonical format

Luau bytecode is a pinned compatibility format owned by another project. Its
instruction set and serialized prototype metadata do not represent every Lua
5.1-5.5 or Blu requirement, including all integer, bitwise, environment,
tail-call, close-variable, finalizer, and source-map semantics. Extending it
privately would create artifacts that look Luau-compatible but are not.
Lowering missing semantics into helpers would make metamethods, yields, error
text, stack behavior, and evaluation order depend on undocumented conventions.

Blu will therefore preserve Luau decoding compatibility without making Luau
serialization the authority over Blu language evolution or portable
artifacts.

## Compatibility and oracle migration

The current FFI adapter will move to an optional `blu-luau-compat` crate. It
will be disabled by default and used by conformance jobs, legacy artifact
reproduction, and an explicitly requested compatibility backend. It will never
be selected silently because the Blu frontend encounters unsupported syntax.

Legacy Luau chunks are decoded and translated into the normalized executable
representation with the `luau` profile. Existing package payloads whose
manifest says `blu` while their format is Luau retain their declared `blu`
profile as a documented legacy rule matching current behavior; such artifacts
cannot contain mixed-profile prototypes.

New compilation APIs accept an explicit source identity and profile and return
a validated Blu artifact with frontend identity, profile, and source digest.
Existing convenience methods may remain as deprecated, documented wrappers.
Newly published source builds use `BluV2`; BluV1 remains readable for existing
artifacts. Existing Luau payloads remain
loadable.

The owned frontend will eventually compile the `luau` profile itself. The
pinned upstream Luau compiler and runtime remain differential oracles, not the
production definition of Blu. The separately pinned bundled compiler release
remains identified as a legacy compiler oracle rather than being conflated
with the pinned Luau compatibility revision.

## Dependency gates

Work may be prototyped in parallel, but support claims follow these gates in
order:

1. **Profile contract:** publish an executable syntax and semantic matrix for
   all seven profiles, including a tested deviation ledger. Unsupported
   features produce structured errors.
2. **Core identity and diagnostics:** establish one profile type, byte-span
   model, source database, and structured diagnostic contract shared by
   compiler, package, bytecode, and runtime.
3. **Artifact and runtime substrate:** implement `BluV1`, encoding, source
   maps, profile-aware validation, Luau translation, and per-frame profiles.
   Hand-built mixed-profile calls must preserve the callee profile.
4. **Syntax:** implement profile-gated lexing and parsing with deterministic
   recovery, acceptance/rejection tests, and parser fuzzing.
5. **First compiler slice:** lower HIR to MIR, allocate registers, emit,
   round-trip, validate, and execute the vertical slice below without linking
   the Luau compiler.
6. **Shared language core:** add scopes, closures, calls, multireturn, varargs,
   tables, ordered assignment, control flow, iteration, metamethods, errors,
   protected calls, and proper tail calls.
7. **Profile deltas:** add Lua 5.2 environments and labels; Lua 5.3 integers,
   floor division, and bitwise operators; Lua 5.4 attributes, closing, and
   finalization; Lua 5.5 declarations and vararg-table behavior; pinned Luau
   syntax and semantics; then the explicitly specified Blu union and
   extensions.
8. **Package migration:** publish Blu-format packages with reproducible
   compiler metadata while retaining explicit legacy loading.
9. **Conformance gate:** advertise a profile only after its applicable syntax,
   execution, library, official-suite, and documented-deviation checks pass.

These gates are subordinate to ADR 0001's secure-execution gate. Source
frontend progress does not permit an artifact to execute before validation,
exceed documented limits, exercise undeclared effects, or change behavior
between embedded and OS-confined placement.

## Smallest vertical slice

The first end-to-end program is:

```lua
--!dialect lua54
local answer = 40
return answer + 2
```

It is intentionally small but exercises directive reconciliation, byte spans,
parsing, binding resolution, local-register allocation, constants, arithmetic,
return, encoding, validation, and execution.

Acceptance requires:

1. compilation takes an explicit source identity and profile;
2. all source compilation uses Blu-owned Rust code;
3. the emitted main prototype contains the selected profile;
4. the artifact round-trips through the Blu encoder, decoder, and opaque
   validator;
5. the VM derives semantics from the executing frame;
6. all seven explicitly selected profiles return `42` against their reference
   runtimes;
7. a conflicting directive reports a structured diagnostic on the directive
   value;
8. malformed syntax reports a stable primary byte span and expected tokens;
9. `//` is rejected for Lua 5.1 and 5.2 and accepted only by profiles whose
   syntax contract includes it;
10. no Luau compiler fallback occurs; and
11. enabling the Luau oracle may compare results but cannot change the chosen
    production backend.

## Conformance requirements

Conformance evidence includes:

- syntax acceptance and error locations checked against each exact Lua
  reference and the pinned Luau parser;
- identical profile-specific source executed by Blu and its reference runtime,
  comparing values, byte output, errors, stack behavior, and libraries;
- separately pinned and hashed official Lua suites for each release and
  applicable pinned Luau conformance and VM suites;
- a portable corpus shared by every profile and targeted divergence corpora
  for integers, environments, tail calls, metamethods, ordering, cleanup,
  finalizers, coroutines, and error paths;
- Blu encoder/decoder round trips, validator mutation tests, and parser,
  artifact, and differential runtime fuzzing; and
- optional comparison with the pinned Luau compiler without requiring
  byte-for-byte equality between Luau and Blu formats.

A profile is `unsupported`, `experimental`, or `conformant`. It becomes
conformant only when applicable suites pass and every intentional deviation is
published. When exact reference error wording is not part of the contract,
tests compare structured category and location instead of unstable prose.

## Consequences

### Benefits

- Blu can represent every promised profile without disguising extensions as
  Luau bytecode.
- Compiler, package, debugger, and runtime share explicit source and profile
  identity.
- Mixed-profile calls follow the callee semantics required by the language
  contract.
- The default build can remove a foreign C++ compiler from the production
  trusted computing base.
- Artifacts remain compatible with ADR 0001's validation, isolation, package,
  and reproducibility requirements.

### Risks and costs

- A production-quality parser, resolver, lowering pipeline, bytecode encoder,
  source map, and diagnostic system are substantial maintenance commitments.
- Profile expansion can produce combinatorial tests and runtime branches.
- A new bytecode format requires package migration and long-term versioning.
- Translating legacy Luau artifacts while introducing per-prototype profiles
  creates a temporary dual-format runtime.
- Incorrect cleanup, multireturn, evaluation-order, or cross-profile behavior
  can appear correct on simple programs; dependency gates and differential
  suites are required to expose it.
- Moving profile selection into frames and native call contexts touches VM,
  coroutine, library, debugger, and package boundaries.

## Non-goals

- Reproducing Luau bytecode byte-for-byte.
- Accepting ambiguous source and guessing its dialect for published artifacts.
- Treating one permissive grammar with runtime switches as profile
  compatibility.
- Hiding unsupported semantics behind helper functions or silent fallback.
- Implementing every profile, optimizer, type checker, standard library, C API,
  JIT, or debugger in the first vertical slice.
- Replacing ADR 0001's validation, authority, resource, worker-isolation, or
  package gates.
