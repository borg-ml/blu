# ADR 0001: Make Blu a portable component runtime

- Status: Draft
- Date: 2026-07-29

## Objective

Blu should provide most of the operational advantages that make WebAssembly
attractive for application extensions, while retaining the advantages of a
live Lua-family language:

- instant source evaluation and hot reload;
- direct, low-latency integration with a Rust host;
- a REPL and inspectable dynamic state;
- Lua and Luau source compatibility;
- a single language, package, debugger, profiler, and lifecycle.

The target is not merely a scripting interpreter. Blu should become a
portable, verifiable, resource-controlled component runtime suitable for local
applications, servers, workers, cloud environments, and eventually browsers.

Wasm is a design benchmark for portability, isolation, resource control,
typed composition, and compiled distribution. Blu does not need to reproduce
Wasm's instruction set or ecosystem, and it must not describe unlike isolation
mechanisms as equivalent. This ADR instead defines scoped operational goals
whose guarantees depend on the selected execution and authority level.

## Context

Blu is intended to be a high-performance, deeply programmable Lua-family
runtime for Borg and other Rust applications. An extension platform built on
it needs:

- fast startup and low idle overhead;
- direct, low-latency host calls;
- interactive evaluation and hot reload;
- portable, validated packages;
- bounded execution and optional crash containment;
- trusted full-user authority when an embedder deliberately grants it;
- compatibility with Lua, Luau, and their package ecosystems;
- a path to native performance for hot code.

WebAssembly Components provide portable compiled artifacts, strong isolation,
multiple implementation languages, typed composition, and an established
interface format. Supporting them alongside Blu would also require a second
runtime, ABI, binding generator, lifecycle, state model, debugger, profiler,
capability adapter, and distribution path. Before accepting that complexity,
Blu should deliberately close every gap that does not fundamentally depend on
Wasm's multi-language ecosystem.

Blu already owns, or is actively implementing, the relevant foundations:

- safe bytecode decoding and strict validation;
- a Rust VM with a generational heap and explicit GC roots;
- instruction, call-stack, register, string, and load limits;
- host functions, globals, and module loading;
- explicit language dialects and authority profiles;
- differential Lua and Luau conformance.

The source compiler currently uses an isolated upstream Luau FFI adapter. Full
Lua frontends, memory quotas, capability enforcement, worker isolation,
packaged bytecode, and native compilation remain incomplete.

## Decision

Blu will be developed as the primary extension **and component** runtime. Its
roadmap will explicitly target these operational properties:

1. portable validated artifacts;
2. typed, versioned imports and exports;
3. safe composition and opaque resources;
4. capability-based host access;
5. deterministic resource limits and interruption;
6. embedded and process-isolated execution;
7. fast instantiation, snapshots, pooling, and hot replacement;
8. AOT/native execution for hot code;
9. reproducible, signed distribution;
10. explicitly versioned placement semantics across supported environments.

Wasm will not be an initial required runtime or package type. It remains a
possible future adapter for the properties Blu cannot reproduce: standardized
multi-language compiled artifacts and direct reuse of the Wasm ecosystem.

The initial execution architecture is:

```text
Rust application
├── Rust built-ins
├── embedded Blu VM
└── optional supervised Blu worker
```

- **Rust built-ins** implement host invariants and realtime or
  performance-critical defaults.
- **Embedded Blu** provides the lowest-latency scripting, live customization,
  and direct host integration.
- **Isolated Blu** runs the same packages and APIs in a supervised process when
  stronger crash containment or policy separation is required.

Isolation is a deployment choice, not a different plugin model. Trusted Blu
code may receive full filesystem, process, network, and native-module
authority. Pure and confined profiles remain available to embedders that need
determinism or restricted access.

The runtime defines four distinct isolation levels. They share package and
service contracts but do not make the same security promises:

| Level | Boundary and intended guarantee |
|---|---|
| Language-safe embedded | Validated Blu bytecode executes in the Rust VM without arbitrary host-memory access. Host bindings remain in the trusted computing base. |
| Capability-confined embedded | Language-safe embedded execution plus explicit, scoped host capabilities and resource budgets. This contains language-level effects but is not a process security boundary. |
| OS-confined worker | The same package and contracts execute across a supervised transport inside platform-supported process, filesystem, network, memory, and CPU controls. |
| Trusted native | In-process host functions or native modules may exercise the full authority of the application or user. This level provides compatibility and performance, not containment. |

Package policy and runtime inspection must state the selected level. A host
must not silently weaken a requested minimum isolation level. Unsupported
platform controls cause explicit placement rejection rather than implicit
behavioral drift.

Native Lua modules remain part of Blu's general compatibility contract.
Loading them in-process requires trusted authority; applications requiring
containment can load them through a supervised worker. This does not require
applications such as Borg to expose native dynamic libraries as their public
plugin ABI.

Public Blu component contracts should be declared independently from their
Rust or Blu bindings and use a constrained canonical type system:

- scalars, strings, byte buffers, lists, records, and variants;
- optional values and structured results;
- opaque, generation-checked resource handles;
- futures, streams, explicit errors, and cancellation;
- versioned service names and schemas.

Schema definitions generate Rust traits, Blu types and bindings,
documentation, compatibility checks, and—if ever needed—WIT adapters. This
preserves a future Wasm path without making WIT or the Component Model an
authority over Blu's direct in-process representation.

## Threat model and trusted computing base

Confined packages, source, bytecode, dependencies, package metadata, and worker
messages are potentially malicious. The runtime must defend against malformed
artifacts, validation bypass, quota evasion, forged or stale handles,
capability escalation, cancellation and reload races, dependency confusion,
and crash-induced partial activation.

At the language-safe and capability-confined embedded levels, the trusted
computing base includes the Rust VM, bytecode decoder and validator, allocator
and GC, generated bindings, every callable host binding, and the embedding
application. The in-process Luau compiler and its FFI are also trusted whenever
they compile untrusted source in the host process. At the worker level, the
supervisor, typed transport, policy evaluator, and operating-system sandbox
configuration join the trusted computing base; the worker runtime and native
modules are treated as compromisable within that boundary.

Blu does not defend against a compromised host application, kernel, trusted
native module, or administrator. Signatures establish artifact identity and
provenance, not safety or authority. Embedded execution cannot contain memory
corruption or arbitrary effects in trusted native code.

## Scoped operational requirements

Blu must describe guarantees per isolation level. It may claim an operational
property only after that property is implemented, adversarially tested, and
measured for the named level; it must not make an undifferentiated claim of
Wasm parity.

### Portable, verifiable artifacts

- A platform-independent, versioned bytecode and package envelope.
- Complete validation before any code executes.
- Execution accepts only an opaque validated-artifact type produced by the
  decoder/validator. Public mutable chunks and prototypes are tooling
  representations and cannot cross an execution boundary without validation.
  Mutation invalidates validation; no safe API may construct or execute an
  artifact by asserting validity.
- Declared dialect, imports, exports, host requirements, and authority.
- Dependency locks, integrity hashes, signatures, source maps, and provenance.
- Reproducible compilation from locked source packages.
- Content-addressed caches and atomic installation.
- Forward-compatible rejection of unsupported bytecode or contracts.
- Separate compatibility guarantees for source, bytecode, and component
  schemas.

### Typed components and composition

- A canonical interface-description format owned by Blu.
- Generated Rust and Blu bindings with no handwritten glue for ordinary
  values.
- Named imports and exports rather than ambient global discovery.
- Semver-compatible interface evolution and generated adapters where safe.
- Opaque resource handles with ownership, borrowing, destruction, and stale
  generation detection.
- Component-to-component linking without routing through application-specific
  APIs.
- Dependency graphs that are resolved and validated before activation.
- No JSON serialization on embedded calls.

### Resource control

- Instruction or fuel accounting.
- Hard heap, stack, call-depth, object-size, handle, output, and task limits.
- Wall-clock deadlines, cooperative interruption, and bounded shutdown.
- Host-call budgets and cancellation propagation across component boundaries.
- Every host call receives an execution context carrying the caller identity,
  effective capabilities, remaining budget, deadline, and cancellation token.
  Bindings must charge attributable CPU/allocation/output/effect costs, poll or
  propagate cancellation, and return structured exhaustion errors.
- Binding contracts declare blocking behavior, thread affinity, suspension,
  and reentrancy. A binding must not block a realtime thread, recursively enter
  a non-reentrant VM, retain borrowed VM memory across suspension, or continue
  externally visible effects after cancellation unless its contract explicitly
  defines idempotency and completion semantics.
- Allocation failure reported as a structured runtime error.
- Per-plugin CPU, allocation, GC-pause, and host-call accounting.
- Deterministic limits that behave equivalently in embedded and worker modes.

### Capabilities, effects, and determinism

- Host access only through explicit, attributable APIs.
- Pure, confined, and trusted authority profiles.
- Package manifests declare authority requirements and optional requests; they
  never grant authority. The host computes the effective grant from user
  consent, policy, placement, and platform support. The effective grant can
  only preserve or reduce requested authority.
- Capability handles that are scoped, delegable, revocable, and auditable.
- Path, command, environment, network-origin, model, and workspace scopes.
- No authority gained merely by importing a module.
- Dependency authority is not unioned into the caller automatically. Delegation
  is explicit, attenuated to a declared scope, attributable to both parties,
  and revocable by the host.
- Virtualizable time, randomness, filesystem, process, and network effects for
  deterministic execution and replay where requested.
- Recorded effect results and stable scheduling points for replay.
- Clear separation between a language compatibility profile and its granted
  host authority.

### Instantiation, state, and hot reload

- Compile and validate a new generation before activation.
- Atomically replace handlers and service implementations.
- Drain or cancel calls using the old generation.
- Roll back when initialization or migration fails.
- Keep authoritative durable state in the host or a versioned state service;
  VM-local state is disposable unless explicitly migrated.
- Preinitialized snapshots for standard libraries and common host bindings.
- Copy-on-write or equivalent snapshot restoration where safe.
- Reusable VM pools for short-lived isolated work.
- Lazy module instantiation and cached linking.
- Explicit initialization, health, quiesce, migrate, and shutdown hooks.

### Isolation

- The same package and host contract must work embedded or in a worker.
- Embedded mode must preserve memory safety for valid Blu programs.
- Bytecode must not access arbitrary host memory or forge resource handles.
- Worker transport must preserve typed errors, cancellation, backpressure,
  capability identity, and resource accounting.
- A worker crash must not corrupt host state or leave a partially activated
  plugin generation.
- Worker processes must support OS-level memory, CPU, filesystem, process, and
  network containment where the host platform permits it.
- Isolation level must be explicit in package policy and observable at runtime.

### Async, concurrency, and streaming

- Structured futures, tasks, streams, and cancellation.
- Bounded channels and explicit backpressure.
- Deterministic scheduling mode for replay and testing.
- Safe suspension across Blu, host, and component calls.
- No blocking host operation on the embedding application's realtime thread.
- Consistent semantics across embedded and isolated execution.

### Performance

- A fast register interpreter for cold and interactive execution.
- Cached compilation and bytecode loading.
- Specialized, generated host bindings without JSON serialization in embedded
  mode.
- Inline caches, quickening, and other interpreter specialization where
  measurements justify them.
- A baseline AOT/JIT tier for hot functions, without changing package
  semantics or observable results.
- Portable bytecode plus architecture-specific, versioned native-code caches.
- Batched APIs and zero-copy borrowed views for large buffers where lifetimes
  can be proved safe.
- Published benchmarks for startup, idle memory, callback latency, host-call
  overhead, throughput, allocation rate, and pause distribution.
- Regression budgets comparing embedded Blu, isolated Blu, Rust built-ins,
  pinned Luau/Lua references, and representative AOT Wasm workloads.

### Distribution and supply chain

- A package manifest, registry format, and dependency lockfile.
- Offline verification of integrity, signature, provenance, and authority
  requirements.
- Reproducible builds and transparent bytecode/compiler version recording.
- Registry policies that distinguish source, generated artifacts, native
  modules, and trusted packages.
- Package inspection that does not execute package code.
- Revocation and vulnerability metadata without silently changing a locked
  build.

### Tooling and observability

- Source-level stack traces through bytecode and native tiers.
- A debugger, profiler, allocation profiler, and host-call profiler.
- Per-component logs, errors, resource usage, and lifecycle state.
- Structured diagnostics suitable for humans, editors, CI, and coding agents.
- Inspection of imports, exports, capabilities, state schemas, and dependency
  graphs.
- Identical package/debug identities across local, worker, and cloud execution.

### Correctness

- Differential conformance against supported Lua and Luau versions.
- Fuzzing of parsing, bytecode validation, execution, GC, host bindings, and
  package loading.
- Structured errors rather than panics for malformed or unsupported programs.
- Stable lifecycle, profiling, and debugging APIs for embedders.
- Cross-platform artifact and execution tests.
- Adversarial tests for quota bypass, handle forgery, capability escalation,
  cancellation races, snapshot corruption, and reload rollback.

### Placement and portability

- Identical package semantics on supported operating systems and architectures.
- An explicit platform-capability query rather than implicit behavioral drift.
- Remote execution without changing component identity or state contracts.
- A browser path by compiling the Blu VM to Wasm, if browser execution becomes
  a product requirement; Blu packages themselves remain Blu artifacts.
- Placement decisions made by the host from capabilities, data locality,
  resource requirements, and policy.

## Dependency-gated roadmap

Later phases must not begin by bypassing an unmet earlier gate. Experimental
work may run in parallel, but it does not become a supported package or
execution path until its dependencies and exit gate are satisfied.

### Phase 1: secure execution substrate

- Introduce the opaque validated-artifact boundary and require it at every VM
  entry point.
- Publish the threat model, trusted computing base, isolation-level contract,
  and platform support matrix.
- Implement hard heap, object, string, output, stack, task, and handle limits;
  deadline and external interruption; and structured allocation failure.
- Introduce the host-call execution context, resource charging, cancellation,
  blocking, thread-affinity, suspension, and reentrancy contracts.
- Fuzz and adversarially test decoding, validation, execution, GC, coroutines,
  host calls, handle lifetime, and quota enforcement.

**Gate:** Malicious artifacts cannot execute without validation, forge live
handles, exceed documented embedded limits, or exercise undeclared effects
except through APIs explicitly classified as trusted native code. Failures are
structured and do not corrupt subsequent VM executions.

### Phase 2: component and package semantics

- Define canonical schemas, stable package and component identities, named
  imports and exports, compatibility rules, and generated Rust/Blu bindings.
- Implement authority requests, host grants, scoped capability handles,
  delegation, attenuation, revocation, and audit records.
- Add the package envelope, dependency resolution and lock, integrity and
  compiler metadata, reproducible-build rules, and inspection without
  execution.
- Implement dependency validation, component linking, lifecycle generations,
  host-owned state, atomic activation, migration, rollback, cancellation, and
  per-component observability in embedded mode.

**Gate:** A locked package can be inspected, policy-checked, linked, bounded,
executed, and atomically replaced in embedded mode without ambient authority or
application-specific binding glue for canonical values.

### Phase 3: isolation, distribution, and optimization

- Implement the supervised worker using the Phase 2 package, interface,
  authority, lifecycle, identity, and observability contracts.
- Add typed transport, backpressure, cancellation, accounting, OS containment,
  crash recovery, and embedded/worker semantic conformance tests.
- Add signatures, provenance, registry and content-addressed cache operations,
  atomic installation, revocation metadata, and cross-platform artifact tests.
- Add snapshots and pools only after capability and handle sanitization rules
  exist. Add remote placement and native-code caches only after semantic
  equivalence is measured.
- Re-evaluate a Wasm adapter and AOT/JIT investment from production evidence
  and published benchmarks rather than treating either as a prerequisite.

**Gate:** An unchanged signed package runs with measured contract, policy, and
lifecycle equivalence in embedded and OS-confined worker modes. Distribution
and optimization do not weaken validation, authority, resource, or rollback
guarantees.

## Operational comparison

| Property | Blu target | Irreducible difference from Wasm |
|---|---|---|
| Portable artifact | Validated Blu package/bytecode | Blu-specific rather than an industry-standard binary |
| Typed interfaces | Canonical Blu component schemas | Smaller external tooling ecosystem |
| Memory safety | Safe VM plus checked handles | VM remains part of the trusted computing base |
| Crash isolation | Supervised Blu worker | Process isolation is heavier than an in-process Wasm instance |
| Resource control | Fuel, quotas, deadlines, OS worker limits | Embedded native host calls require host cooperation |
| Fast startup | Cached bytecode, snapshots, pools | Must be implemented and benchmarked by Blu |
| Hot reload | Atomic generations and host-owned state | Blu should be more interactive than Wasm |
| Native speed | AOT/JIT caches and specialized host calls | New compiler maturity trails established Wasm engines |
| Multi-language input | External processes or native modules | Wasm supports many compiled source languages directly |
| Ecosystem portability | Blu registry and host adapters | Cannot reuse arbitrary existing Wasm components |
| Browser execution | Blu VM compiled to Wasm | Adds a Wasm host beneath Blu in that environment |

## What Blu does not replace

Even after these requirements are met, Wasm retains distinct advantages:

- standardized compiled components authored in many languages;
- reuse of the broader Wasm toolchain and component ecosystem;
- a mature portable AOT compilation and isolation substrate;
- standardized deployment across unrelated hosts.

Blu should not recreate those ecosystems merely to eliminate Wasm. Conversely,
those advantages do not justify carrying Wasm before an embedder has a real
multi-language component requirement.

## Definition of success

Blu has met this ADR's scoped operational goals when a signed package can be:

1. compiled reproducibly and validated without executing it;
2. inspected for interfaces, capabilities, dependencies, and limits;
3. run unchanged embedded or in a supervised worker;
4. interrupted and bounded for CPU, memory, stack, output, and host effects;
5. linked to typed host and component services without JSON;
6. hot-reloaded atomically with state migration and rollback;
7. profiled and debugged at source level;
8. moved between local and remote hosts without changing its identity;
9. accelerated through a versioned native-code cache;
10. demonstrated by adversarial tests and published performance results.

This definition intentionally excludes multi-language compilation and reuse of
the Wasm ecosystem. Those are Wasm's durable unique advantages, not properties
Blu should imitate inefficiently.

## Reconsidering Wasm

Add a Wasm adapter only when at least one production use case demonstrates a
need that Blu, a Rust built-in, or an isolated Blu worker cannot reasonably
satisfy. Examples include:

- distributing third-party components written in Rust, C++, Zig, or Go;
- executing the same established component in unrelated Wasm hosts;
- browser or cloud deployment that specifically requires a standard Wasm
  artifact;
- reusing a substantial existing Wasm component ecosystem;
- compute workloads where Blu's native tier remains materially insufficient;
- an isolation requirement better served by a mature Wasm engine than a
  process worker.

Any future adapter must use the same package identity, service contracts,
authority model, state store, lifecycle, and observability as Blu. It must not
create a second plugin system.

## Consequences

### Benefits

- One primary language, package model, debugger, profiler, and reload model.
- No Wasm runtime cost or complexity for applications that do not need it.
- Blu can optimize its host boundary specifically for dynamic extension work.
- Embedded and isolated execution cover both low latency and containment.
- The architecture remains open to Wasm when evidence justifies it.

### Costs and risks

- Blu must implement component schemas, resource control, package
  verification, snapshots, worker isolation, and native compilation rather
  than inheriting them from Wasm.
- A new VM initially has less security and performance maturity than Wasmtime.
- In-process safety depends on the Blu runtime and every exposed host binding.
- Multi-language compiled extensions remain external processes or Rust
  built-ins until a Wasm adapter exists.
- Owning these capabilities is a substantial long-term runtime commitment.
