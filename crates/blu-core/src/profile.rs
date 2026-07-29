use core::{fmt, str::FromStr};

/// An explicit language and runtime semantic profile.
///
/// `Luau` means the revision pinned by the repository's `UPSTREAM.toml`; it
/// does not mean the separately pinned bootstrap source-compiler release.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum SemanticProfile {
    Blu,
    Luau,
    Lua51,
    Lua52,
    Lua53,
    Lua54,
    Lua55,
}

impl SemanticProfile {
    pub const ALL: [Self; 7] = [
        Self::Blu,
        Self::Luau,
        Self::Lua51,
        Self::Lua52,
        Self::Lua53,
        Self::Lua54,
        Self::Lua55,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blu => "blu",
            Self::Luau => "luau",
            Self::Lua51 => "lua51",
            Self::Lua52 => "lua52",
            Self::Lua53 => "lua53",
            Self::Lua54 => "lua54",
            Self::Lua55 => "lua55",
        }
    }
}

impl fmt::Display for SemanticProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SemanticProfile {
    type Err = ParseSemanticProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "blu" => Ok(Self::Blu),
            "luau" => Ok(Self::Luau),
            "lua51" => Ok(Self::Lua51),
            "lua52" => Ok(Self::Lua52),
            "lua53" => Ok(Self::Lua53),
            "lua54" => Ok(Self::Lua54),
            "lua55" => Ok(Self::Lua55),
            _ => Err(ParseSemanticProfileError(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSemanticProfileError(String);

impl ParseSemanticProfileError {
    #[must_use]
    pub fn value(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParseSemanticProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown semantic profile {:?}", self.0)
    }
}

impl std::error::Error for ParseSemanticProfileError {}
