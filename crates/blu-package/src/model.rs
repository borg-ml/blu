use core::fmt;

pub use blu_core::SemanticProfile;

/// Backwards-compatible name for a package's semantic profile.
pub type PackageDialect = SemanticProfile;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest(pub(crate) [u8; 32]);

impl Digest {
    pub const ZERO: Self = Self([0; 32]);

    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Name(String);

impl Name {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        validate_name(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_validated(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

pub(crate) fn validate_name(value: &str) -> Result<(), &'static str> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err("name is empty");
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err("name must start with a lowercase ASCII letter or digit");
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return Err("name must end with a lowercase ASCII letter or digit");
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(byte))
    {
        return Err("name contains a non-portable character");
    }
    if value.split('.').any(str::is_empty) {
        return Err("name contains an empty dot-separated segment");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum BytecodeFormat {
    Luau = 1,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BytecodeDescriptor {
    pub format: BytecodeFormat,
    pub version: u8,
    pub typeinfo_version: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIdentity {
    pub name: Name,
    pub version: Version,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ImportSource {
    Host = 1,
    Component = 2,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServiceId {
    pub namespace: Name,
    pub name: Name,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Import {
    pub source: ImportSource,
    pub service: ServiceId,
    pub version: Version,
    pub schema: Digest,
    pub optional: bool,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Export {
    pub service: ServiceId,
    pub version: Version,
    pub schema: Digest,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum AuthorityProfile {
    Pure = 1,
    Confined = 2,
    Trusted = 3,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityRequirement {
    pub name: Name,
    /// Host-defined canonical scope bytes. This declaration grants no access.
    pub scope: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityRequirement {
    pub profile: AuthorityProfile,
    pub capabilities: Vec<CapabilityRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    pub package: PackageIdentity,
    pub dialect: SemanticProfile,
    pub bytecode: BytecodeDescriptor,
    pub authority: AuthorityRequirement,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
}
