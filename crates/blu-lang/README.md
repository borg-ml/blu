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

Blu is under active compatibility development. The current source path uses a
pinned Luau compiler and a Blu VM implemented without unsafe Rust. Explicit Lua
5.1–5.5 profiles are declared but are not yet implemented; unsupported
behavior fails explicitly.

See the [repository README](https://github.com/borg-ml/blu) for current
capabilities, embedding details, compatibility scope, and upstream attribution.
