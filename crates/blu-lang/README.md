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

Blu is under active compatibility development. The current source path uses a
pinned Luau compiler and a Blu VM implemented without unsafe Rust. Explicit Lua
5.1–5.5 profiles are declared but are not yet implemented; unsupported
behavior fails explicitly.

See the [repository README](https://github.com/borg-ml/blu) for current
capabilities, embedding details, compatibility scope, and upstream attribution.
