#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Dialect {
    #[default]
    Blu,
    Luau,
    Lua51,
    Lua52,
    Lua53,
    Lua54,
    Lua55,
}
