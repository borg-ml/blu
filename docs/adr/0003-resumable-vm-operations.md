# ADR 0003: Represent yieldable VM operations as explicit continuations

- Status: Accepted
- Date: 2026-07-29

## Context

Blu executes ordinary bytecode calls on an owned frame stack. A suspended
coroutine retains its active frame, saved callers, registers, open upvalues,
protected-call state, and semantic execution context. This is sufficient when
the bytecode operation that invokes user code is an ordinary call.

Many Lua-family operations can invoke user code without appearing as a source
or bytecode `CALL`:

- generic iterator preparation and steps;
- `__index` and `__newindex`;
- arithmetic, ordering, equality, concatenation, and length metamethods;
- callable values;
- future library callbacks, such as a `table.sort` comparator or `string.gsub`
  replacement function.

The current bootstrap runtime implements several of these by calling into a
closure from the Rust implementation of the enclosing operation. If that
closure yields, the callee can preserve its own frames, but the VM also needs
to remember what operation invoked it, where its results belong, and what work
must occur after it returns. Re-entering the operation from its beginning is
incorrect: it can repeat table accesses, metamethod lookup, iterator steps,
comparisons, callbacks, or other observable effects.

Using Rust recursion to retain this state would make coroutine depth depend on
the host stack, obscure GC roots, complicate protected-error unwinding, and
prevent deterministic limits. Ad hoc state for each opcode would duplicate
call, yield, error, and root-handling logic and would make future library
callbacks another incompatible suspension mechanism.

[ADR 0001](0001-blu-portable-component-runtime.md) requires safe suspension,
deterministic resource control, and equivalent semantics in embedded and
worker placement. [ADR 0002](0002-blu-owned-frontend.md) requires every frame,
closure, continuation, prototype, and native execution context to retain the
callee's semantic profile. Resumable operations must preserve both contracts.

## Decision

Every VM operation that can invoke yieldable code will be represented as an
explicit operation continuation in the owned frame engine.

The first implemented non-ordinary-call consumer is BluV1 final-call
table-list expansion: the caller record retains the destination table and
next array index, then consumes the callee's complete result vector after a
Blu closure or native function returns. This establishes the caller/operation
combination permitted below; iterator, metamethod, protected-call, and library
callback continuations remain incremental follow-up work.

Before invoking user code, the VM records enough owned state to do exactly one
of the following when the invocation finishes:

1. consume successful results and complete the suspended operation;
2. propagate or transform an error according to the operation and active
   protected boundary; or
3. retain the operation unchanged while the callee is suspended.

The VM does not depend on the Rust call stack to remember pending language
operations. Yield, resume, error propagation, protected unwinding, GC tracing,
and resource accounting operate over the same explicit continuation stack
used by ordinary calls.

### Logical continuation shape

The concrete Rust representation may evolve, but the runtime model contains
the following logical state:

```text
Suspended execution
├── active callee frame
├── saved caller frames
├── pending operation stack
└── resume destination

Pending operation
├── operation kind
├── phase
├── originating frame and semantic profile
├── owned operands and intermediate values
├── result or control-flow destination
├── protected/unwind context
└── operation-specific progress
```

An implementation may combine caller and operation records, or store an
operation on its owning frame, provided the observable invariants in this ADR
hold. A pending operation is not a Rust closure and does not contain borrowed
references into a register vector, table storage, bytecode buffer, or native
stack frame.

#### Common fields

Each pending operation logically retains:

- **kind:** the semantic operation being completed;
- **phase:** the point after which execution will resume, so completed effects
  are not repeated;
- **profile:** the profile of the prototype that initiated the operation;
- **operands:** values whose evaluation was completed before the call;
- **destination:** register, result arity, branch target, loop state, or
  library-result slot that consumes the callee result;
- **progress:** any accumulator, cursor, range, or callback index needed to
  continue without restarting; and
- **unwind classification:** whether an error is propagated directly,
  converted by a protected call, passed to an error handler, or handled by an
  operation-specific rule.

Operands that have already been evaluated are captured as owned VM values.
The continuation must not re-read a source register when doing so would observe
a mutation that occurred during the invocation. Destinations may remain
logical register or control-flow coordinates because they are not read until
the operation completes.

### Operation states

The first required operation kinds are:

| Operation | State retained across the invocation | Successful completion |
|---|---|---|
| Ordinary call | Function, arguments, result destination and arity | Apply normal multireturn adjustment |
| Generic-for preparation | Iterator source, selected protocol, loop registers and entry target | Install iterator function, state, control value, and profile-defined extra state |
| Generic-for step | Iterator function, state, control value, result-variable range and loop target | Store results, update control value, and branch when the first result is non-nil |
| Index metamethod | Receiver, key, lookup phase and result register | Store the first result, or continue a profile-defined table chain |
| New-index metamethod | Receiver, key, assigned value and lookup phase | Complete assignment, or continue a profile-defined table chain |
| Callable value | Callable receiver, resolved handler, original arguments and result destination | Apply the result as an ordinary call |
| Arithmetic metamethod | Operator/event, evaluated left and right operands, selected handler and result register | Store the first result after profile-required validation |
| Comparison metamethod | Comparison/event, operands, selected handler, inversion/fallback state and branch target | Convert the first result to the profile-defined truth value and branch |
| Concatenation metamethod | Remaining operand range, accumulated right value, selected handler and result register | Continue the right-to-left concatenation sequence |
| Length metamethod | Operand, selected handler and result register | Validate and store the profile-defined length result |
| Library callback | Stable builtin operation identity, callback role, owned arguments, destination and progress | Continue the builtin state machine from the next phase |

This table specifies semantic information, not a required one-variant-per-row
Rust enum. Operations may share representations when they have identical
resume and unwind behavior.

Library callbacks use a stable builtin operation identity and serializable or
otherwise owned progress state. They do not preserve an arbitrary Rust closure
or a borrow into a native function. For example, a sort continuation may need
the collection identity, algorithm phase, indices, comparator, and pending
comparison destination; a substitution continuation may need the subject,
pattern-machine position, captures, replacement callback, and output
accumulator. The individual library algorithm remains a separate decision.

### Suspension and resumption

Invocation follows one state transition:

```text
evaluate operands
  -> resolve profile-defined handler/protocol
  -> install pending operation
  -> enter callee frame
  -> callee returns, errors, or yields
```

The pending operation is installed before the callee can run. A yield therefore
cannot occur in a window where the callee is suspended but its owner is absent.

On yield:

- the callee frame, saved callers, and all pending operations become part of
  the thread continuation;
- no post-call phase of the owning operation runs;
- yielded values are returned to the resumer under the active profile's
  coroutine rules; and
- resuming the thread supplies values to the suspended call site, not to the
  beginning of the enclosing operation.

On successful callee return:

- its result list is delivered to the top pending operation;
- that operation performs exactly its recorded next phase;
- it either completes, invokes another callee after installing another pending
  state, or advances to a later phase; and
- nested operations are processed in last-in, first-out order.

An operation may suspend repeatedly. Its completed phases and progress are
retained between suspensions. Resume must not redo metamethod selection,
iterator protocol selection, earlier comparisons, earlier replacements, or
already committed table effects unless the selected language profile defines
that repetition.

### Errors and protected unwinding

Errors use the same explicit unwind path whether they occur before a yield or
after one or more resumes.

When a callee or resumed operation errors:

1. discard the failing callee and pending operations up to the nearest
   applicable protected boundary;
2. preserve error values according to the active callee and boundary profiles;
3. produce `false, error` for `pcall`, or invoke the saved `xpcall` handler;
4. if an error handler yields, install its own pending operation before
   suspension and retain the outer callers; and
5. terminate the thread only when no protected boundary handles the error.

Discarding a pending operation does not execute its successful completion
phase. Profile-defined cleanup, close-variable, or finalizer actions are
separate unwind operations and must run in their specified order once those
features are implemented.

An error raised by an `xpcall` handler is not caught again by the same
`xpcall`. It continues outward to the next protected boundary. A handler that
yields and later returns completes the original `xpcall` as
`false, handled_value`.

Operation continuations must not translate all runtime failures into language
errors indiscriminately. Resource exhaustion, host cancellation, placement
failure, and other embedding conditions follow their separately declared
catchability contract.

### GC roots and owned state

Every VM value retained by a pending operation is a GC root for as long as the
operation can resume. This includes:

- receiver, key, assigned value, and metamethod;
- iterator function, state, control value, and extra iterator state;
- arithmetic, comparison, concatenation, and length operands or accumulators;
- library callback, captures, subject values, comparator state, and partial
  results;
- error handlers and protected error values; and
- closures, threads, tables, userdata, buffers, or future heap values reachable
  from any of the above.

Root discovery traverses active frames, saved callers, pending operations, and
thread continuations. The VM must be safe if a native function triggers
collection immediately before a yield, while suspended, during an error
handler, or immediately after resumption.

Pending state owns values or uses generation-checked heap handles. It never
retains raw pointers or borrows into movable or mutable VM storage. Dropping an
operation during unwind releases its roots only after all required cleanup
state has been transferred or completed.

### Call depth, fuel, and interruption

A language call entered by a pending operation consumes call depth in the same
way as an equivalent ordinary call. Resuming an existing call does not consume
an additional call-depth unit. Repeated operation phases consume instruction
fuel or another explicit VM work charge; suspension does not replenish a
budget unless the embedding contract explicitly starts a new budget.

The implementation must therefore satisfy:

- call depth depends on live language frames, not Rust recursion;
- a chain of metamethods, iterators, or callbacks cannot evade the call limit;
- operation dispatch and post-call phases are interruptible and charged;
- an interrupted operation retains no partially installed, unrooted state;
- resumption does not double-count already entered frames; and
- a deep operation chain fails structurally at configured limits rather than
  overflowing the Rust stack.

Proper tail calls are a related but separate frame-replacement decision. A
pending operation must not prevent tail-call replacement when the selected
profile permits it, and tail replacement must not discard an outstanding
operation owned by the caller.

### Profiles and cross-profile calls

ADR 0002's per-prototype profile rule applies at every transition.

- Handler lookup, operand selection, yieldability, result validation, fallback,
  and error behavior use the profile of the frame that initiated the
  operation.
- The invoked closure executes under its own prototype profile.
- On return, completion resumes under the initiating operation's saved
  profile.
- A native callback receives an execution context containing the active caller
  profile; it does not consult a VM-global dialect.
- Cross-profile values retain identity while the caller and callee apply their
  own observable rules.

Profiles may disagree about whether a particular boundary is yieldable. For
example, Lua 5.1 compatibility can reject a yield across a protected or
metamethod boundary where Luau or a later Lua profile permits suspension. That
decision is recorded in the pending operation before entering the callee. A
disallowed yield becomes the profile-defined boundary error and does not leave
a resumable continuation.

Iteration protocol selection is also profile-owned: Luau `__iter`, Lua
5.2–5.5 `__pairs`, Lua 5.5's additional iterator state, and raw `next`
iteration must not be collapsed into one inferred behavior.

### Native and host calls

This ADR does not make every native call yieldable automatically. A host
binding declares whether it is synchronous, resumable, blocking, cancellable,
or reentrant as required by ADR 0001.

A resumable host call uses an explicit host-operation token and owned VM state.
It cannot retain borrowed VM memory across suspension. Completion re-enters the
same pending-operation machinery as a language callee. A synchronous binding
that attempts to yield across a forbidden boundary produces a structured
profile or host-contract error.

## Consequences

### Positive

- All language-level suspension uses one explicit frame and continuation
  model.
- Yielding iterators, metamethods, and future library callbacks preserve their
  enclosing operations.
- Protected errors behave identically before and after resumption.
- GC roots and resource limits are inspectable rather than implicit in Rust
  stack frames.
- Mixed-profile calls retain both callee execution semantics and caller
  completion semantics.
- The same operation state can support embedded execution, worker placement,
  debugging, profiling, and later native tiers.

### Costs and risks

- More VM operations become multi-phase state machines.
- Saved continuations grow by operation-specific state and require explicit
  tracing.
- Library algorithms that call user code cannot be implemented as ordinary
  monolithic Rust functions.
- Debuggers and profilers must distinguish language frames from pending
  operation frames without inventing source calls that did not occur.
- Incorrect phase boundaries can duplicate or omit observable effects.
- Cross-profile tests are required even when the callee source is identical.

## Dependency gates

The following gates precede advertising generalized iteration, yieldable
metamethods, or callback-driven libraries as compatible:

1. **Common invocation outcome:** ordinary calls and non-`CALL` operations
   report success, error, and yield through one semantic outcome path.
2. **Owned operation state:** every required operation kind has an owned,
   traceable state with no Rust-stack or borrowed-memory dependency.
3. **Nested suspension:** an operation can invoke another operation, yield
   repeatedly, and resume in last-in, first-out order.
4. **Protected unwind:** errors before and after resumption reach the same
   nearest `pcall`/`xpcall` boundary; yielding handlers retain outer callers.
5. **Profile propagation:** hand-built mixed-profile fixtures demonstrate
   callee-profile execution and caller-profile completion.
6. **GC safety:** collection at every call/yield/resume/error boundary retains
   all operation operands, intermediates, handlers, and destinations.
7. **Limit safety:** deep iterator/metamethod/callback chains hit configured VM
   limits without Rust stack overflow or budget reset.
8. **Observability:** disassembly, stack traces, debugger state, and profiler
   events can identify a pending operation without exposing representation
   internals.

Tail-call replacement, weak/finalizer semantics, and individual library
algorithms may proceed separately, but cannot bypass these gates when they
invoke yieldable code.

## Cross-thread coroutine activation

`coroutine.wrap` exposes a distinct continuation problem from a yield inside
one coroutine: a wrapped coroutine can synchronously call another wrapped
coroutine, causing `call_value` to re-enter `resume_thread` before the outer
activation has completed. The owned BluV1 path now represents this pending
cross-thread resume with `drive_blu_coroutine`, an explicit iterative
activation stack. Each entry retains:

- the target thread and its resumable state;
- the caller activation and result destination waiting for that thread;
- the active profile, fuel, call-depth accounting, and protected/error
  boundary;
- all arguments and yielded/resumed values; and
- GC roots for every activation, pending continuation, closure, thread, and
  error value.

The scheduler iteratively drives one activation to return, yield, or error,
then delivers that outcome to the waiting activation without recursive
`resume_thread` re-entry. A nested resume consumes one live language
activation, while resuming an existing frame does not consume another frame;
call-limit accounting preserves that distinction. Error unwinding and
`pcall`/`xpcall` handling pop or retain activation entries using the same rules
as ordinary frames, and collection traces the complete activation stack. The
pinned Lua 5.1 portable child matrix now passes all eight cases, including
`test/sieve.lua`.

`MAX_NESTED_COROUTINE_RESUMES` remains a deliberate 64-level guard only for
the legacy/foreign synchronous bridge, while `BluResume::Coroutine` reached
from an arbitrary suspended cross-thread continuation remains an explicit
structured unsupported-feature boundary. Increasing that bridge guard would
not implement the missing foreign continuation ownership.

## Conformance and regression tests

The primary oracle is the pinned Luau revision recorded in `UPSTREAM.toml`.
Implementation proceeds by admitting complete applicable sections from:

- `tests/conformance/iter.luau`, including table `__iter`, yielding iterator
  preparation, yielding iterator functions, errors, and repeated resumes; its
  C++-only `cYieldingIterator` callback is tracked as a host-capability
  isolation;
- `tests/conformance/events.luau`, including index, assignment, arithmetic,
  comparison, concatenation, length, handler selection, and error cases;
- `tests/conformance/cyield.luau`, including yields across protected and native
  boundaries;
- `tests/conformance/coroutine.luau`, including nested resume/yield, terminal
  states, and close behavior; and
- `tests/conformance/pcall.luau` where protected behavior intersects operation
  suspension.

Focused Blu regressions additionally cover:

1. yield and resume once and repeatedly inside each operation kind;
2. resume values consumed by the callee before the owning operation completes;
3. error before yield, after resume, and after multiple resumes;
4. nested `pcall` and `xpcall`, including an error handler that yields or
   errors;
5. an inner operation yielding while an outer operation is pending;
6. GC triggered before invocation, while suspended, inside a callback, inside
   an error handler, and immediately after completion;
7. deep chains under call, instruction, stack, and heap limits with no Rust
   panic or stack overflow;
8. mutation of tables or globals during a suspended operation, proving that
   completed phases are not repeated and captured operands are not
   accidentally re-read;
9. multireturn truncation, padding, and forwarding into each destination kind;
10. profile pairs that intentionally disagree about yieldability, iteration,
    metamethod selection, result validation, or error behavior; and
11. thread status and terminal error state after caught and uncaught operation
    failures.

Official Lua 5.1–5.5 tests and source implementations are additional oracles
once those profiles execute. Tests record reference version, profile, values,
output, errors, stack behavior, and yield points. A passing Luau case does not
authorize the same behavior for a Lua compatibility profile.

## Rejected alternatives

### Preserve suspension with Rust recursion

Rejected because host-stack depth would become a language limit, roots would
remain implicit, and deep or adversarial programs could overflow the process
stack.

### Restart the enclosing opcode after resume

Rejected because lookup, iterator, metamethod, callback, mutation, and error
effects could execute more than once.

### Make only bytecode `CALL` yieldable

Rejected because pinned Luau and later Lua profiles permit yields through
operations that invoke user code indirectly.

### Add bespoke continuation fields for each current opcode

Rejected as the architectural contract. Operation-specific payloads are
necessary, but suspension, outcomes, roots, limits, profiles, and unwinding
must share one model that also supports future library and host callbacks.

### Store arbitrary native closures as continuation state

Rejected because such closures can hide borrowed memory, untraced VM values,
authority, and unaccounted work, and cannot be inspected or transferred safely.

## Acceptance criteria

This ADR is implemented when:

1. every supported yieldable non-`CALL` invocation installs explicit owned
   operation state before entering user code;
2. nested and repeated suspension completes each operation exactly once;
3. no supported yield path relies on Rust recursion or borrowed native state;
4. GC and configured limits remain correct at every transition;
5. protected errors have identical outcomes before and after resumption;
6. caller and callee profiles are preserved across mixed-profile operations;
7. the applicable pinned `iter`, `events`, `cyield`, `coroutine`, and `pcall`
   cases pass differentially; and
8. no compatibility profile is promoted beyond the evidence recorded in the
   dialect matrix.
