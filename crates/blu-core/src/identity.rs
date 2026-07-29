use core::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct SourceId(u32);

impl SourceId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "source:{}", self.0)
    }
}

/// Stable opaque compiler identity bytes recorded in an artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CompilerId([u8; 16]);

impl CompilerId {
    #[must_use]
    pub const fn new(value: [u8; 16]) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityLimits {
    pub max_source_name_bytes: usize,
    pub max_compiler_name_bytes: usize,
    pub max_compiler_version_bytes: usize,
    pub max_compiler_revision_bytes: usize,
}

impl Default for IdentityLimits {
    fn default() -> Self {
        Self {
            max_source_name_bytes: 4 * 1024,
            max_compiler_name_bytes: 256,
            max_compiler_version_bytes: 256,
            max_compiler_revision_bytes: 256,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceIdentity {
    id: SourceId,
    name: String,
}

impl SourceIdentity {
    pub fn new(
        id: SourceId,
        name: impl Into<String>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let name = name.into();
        validate_text("source name", &name, limits.max_source_name_bytes, false)?;
        Ok(Self { id, name })
    }

    #[must_use]
    pub const fn id(&self) -> SourceId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompilerIdentity {
    id: CompilerId,
    name: String,
    version: String,
    revision: Option<String>,
}

impl CompilerIdentity {
    pub fn new(
        id: CompilerId,
        name: impl Into<String>,
        version: impl Into<String>,
        revision: Option<String>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let name = name.into();
        let version = version.into();
        validate_text(
            "compiler name",
            &name,
            limits.max_compiler_name_bytes,
            false,
        )?;
        validate_text(
            "compiler version",
            &version,
            limits.max_compiler_version_bytes,
            false,
        )?;
        if let Some(revision) = &revision {
            validate_text(
                "compiler revision",
                revision,
                limits.max_compiler_revision_bytes,
                false,
            )?;
        }
        Ok(Self {
            id,
            name,
            version,
            revision,
        })
    }

    #[must_use]
    pub const fn id(&self) -> CompilerId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }
}

fn validate_text(
    what: &'static str,
    value: &str,
    limit: usize,
    allow_empty: bool,
) -> Result<(), IdentityError> {
    if !allow_empty && value.is_empty() {
        return Err(IdentityError::Empty { what });
    }
    if value.len() > limit {
        return Err(IdentityError::TooLarge {
            what,
            actual: value.len(),
            limit,
        });
    }
    if let Some(index) = value.bytes().position(|byte| byte == 0) {
        return Err(IdentityError::Nul { what, index });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityError {
    Empty {
        what: &'static str,
    },
    TooLarge {
        what: &'static str,
        actual: usize,
        limit: usize,
    },
    Nul {
        what: &'static str,
        index: usize,
    },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { what } => write!(formatter, "{what} is empty"),
            Self::TooLarge {
                what,
                actual,
                limit,
            } => write!(
                formatter,
                "{what} contains {actual} bytes, exceeding limit {limit}"
            ),
            Self::Nul { what, index } => {
                write!(formatter, "{what} contains a NUL byte at offset {index}")
            }
        }
    }
}

impl std::error::Error for IdentityError {}
