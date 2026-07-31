# blu-lang

`blu-lang` is the public Rust embedding facade for
[Blu](https://github.com/borg-ml/blu), a Lua/Luau superset language and
runtime.

```rust
use blu_lang::{Engine, Value};

let values = Engine::default()
    .execute("return 20 + 22")
    .expect("valid Blu source");
assert_eq!(values, vec![Value::Number(42.0)]);
```

Tables, closures, and threads returned from execution remain rooted across
later VM calls. After the host is finished with them, release their retention
entries with `engine.vm_mut().release_values(&values)`. The default VM retains
at most 4096 returned heap-handle occurrences; configure that bound with
`Vm::with_host_value_limit`. Heap handles cloned from `Vm::global` or read via
`Vm::heap` require an explicit `Vm::retain_value` call if they may outlive their
existing VM root. Host-side `Value` clones do not create additional retention
entries; release exactly once per returned or explicitly retained occurrence.

Blu is under active compatibility development. The legacy source path uses a
pinned Luau compiler and a Blu VM implemented without unsafe Rust. The
profile-aware `Engine::execute_owned_source` path exposes the bounded owned
frontend baseline for Lua 5.1–5.5 as well as Blu and Luau; unsupported behavior
still fails explicitly outside that implemented slice.

See the [repository README](https://github.com/borg-ml/blu) for current
capabilities, embedding details, compatibility scope, and upstream attribution.

Portable package execution checks declared capability requirements against the
host policy using exact opaque name-and-scope matches. This permits a confined
or trusted package to pass its authority gate, but does not yet create
delegable capability handles or link imported services.
