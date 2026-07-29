use crate::{
    AuthorityProfile, AuthorityRequirement, BytecodeDescriptor, BytecodeFormat,
    CapabilityRequirement, Digest, Export, Import, ImportSource, Manifest, Name, PACKAGE_FORMAT_V1,
    PackageDialect, PackageIdentity, ServiceId, Version,
};
use blu_bytecode::{Chunk, ChunkError, LoadLimits, load};
use core::fmt;
use sha2::{Digest as _, Sha256};

const MAGIC: &[u8; 8] = b"BLUPKG\r\n";
const DOMAIN: &[u8] = b"blu.package.v1\0";
const HEADER_BYTES: usize = 8 + 2 + 4 + 8;
const DIGEST_BYTES: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct PackageLimits {
    pub max_bytes: usize,
    pub max_manifest_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_name_bytes: usize,
    pub max_imports: usize,
    pub max_exports: usize,
    pub max_capabilities: usize,
    pub max_scope_bytes: usize,
    pub bytecode: LoadLimits,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            max_bytes: 66 * 1024 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
            max_name_bytes: 128,
            max_imports: 4096,
            max_exports: 4096,
            max_capabilities: 1024,
            max_scope_bytes: 4096,
            bytecode: LoadLimits::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    manifest: Manifest,
    payload: Vec<u8>,
    digest: Digest,
    chunk: Chunk,
}

impl Package {
    pub fn new(
        manifest: Manifest,
        payload: Vec<u8>,
        limits: PackageLimits,
    ) -> Result<Self, PackageError> {
        validate_manifest(&manifest, &limits)?;
        limit("payload bytes", payload.len(), limits.max_payload_bytes)?;
        let chunk = load(&payload, limits.bytecode).map_err(PackageError::Bytecode)?;
        validate_descriptor(&manifest, &chunk)?;
        let manifest_bytes = encode_manifest(&manifest)?;
        limit(
            "manifest bytes",
            manifest_bytes.len(),
            limits.max_manifest_bytes,
        )?;
        let unsigned = encode_unsigned(&manifest_bytes, &payload)?;
        let total = unsigned
            .len()
            .checked_add(DIGEST_BYTES)
            .ok_or(PackageError::LengthOverflow)?;
        limit("package bytes", total, limits.max_bytes)?;
        let digest = hash(&unsigned);
        Ok(Self {
            manifest,
            payload,
            digest,
            chunk,
        })
    }

    pub fn decode(bytes: &[u8], limits: PackageLimits) -> Result<Self, PackageError> {
        limit("package bytes", bytes.len(), limits.max_bytes)?;
        if bytes.len() < HEADER_BYTES + DIGEST_BYTES {
            return Err(PackageError::Truncated);
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(PackageError::InvalidMagic);
        }
        let mut reader = Reader::new(&bytes[MAGIC.len()..]);
        let format = reader.u16()?;
        if format != PACKAGE_FORMAT_V1 {
            return Err(PackageError::UnsupportedFormat(format));
        }
        let manifest_len = reader.usize_u32()?;
        let payload_len = reader.usize_u64()?;
        limit("manifest bytes", manifest_len, limits.max_manifest_bytes)?;
        limit("payload bytes", payload_len, limits.max_payload_bytes)?;
        let unsigned_len = HEADER_BYTES
            .checked_add(manifest_len)
            .and_then(|value| value.checked_add(payload_len))
            .ok_or(PackageError::LengthOverflow)?;
        let expected_len = unsigned_len
            .checked_add(DIGEST_BYTES)
            .ok_or(PackageError::LengthOverflow)?;
        if bytes.len() < expected_len {
            return Err(PackageError::Truncated);
        }
        if bytes.len() > expected_len {
            return Err(PackageError::TrailingBytes(bytes.len() - expected_len));
        }
        let manifest_start = HEADER_BYTES;
        let payload_start = manifest_start + manifest_len;
        let digest_start = payload_start + payload_len;
        let expected = hash(&bytes[..digest_start]);
        let mut stored = [0; DIGEST_BYTES];
        stored.copy_from_slice(&bytes[digest_start..]);
        if !constant_time_eq(expected.as_bytes(), &stored) {
            return Err(PackageError::Integrity {
                expected,
                actual: Digest::new(stored),
            });
        }
        let manifest = decode_manifest(&bytes[manifest_start..payload_start], &limits)?;
        validate_manifest(&manifest, &limits)?;
        let payload = bytes[payload_start..digest_start].to_vec();
        let chunk = load(&payload, limits.bytecode).map_err(PackageError::Bytecode)?;
        validate_descriptor(&manifest, &chunk)?;
        Ok(Self {
            manifest,
            payload,
            digest: expected,
            chunk,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let manifest = encode_manifest(&self.manifest).expect("validated manifest must encode");
        let mut bytes =
            encode_unsigned(&manifest, &self.payload).expect("validated package must encode");
        bytes.extend_from_slice(self.digest.as_bytes());
        bytes
    }

    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    #[must_use]
    pub const fn chunk(&self) -> &Chunk {
        &self.chunk
    }

    #[must_use]
    pub fn into_chunk(self) -> Chunk {
        self.chunk
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageError {
    TooLarge {
        what: &'static str,
        actual: usize,
        limit: usize,
    },
    InvalidMagic,
    UnsupportedFormat(u16),
    Truncated,
    TrailingBytes(usize),
    LengthOverflow,
    InvalidUtf8,
    InvalidName(&'static str),
    UnknownTag {
        what: &'static str,
        value: u8,
    },
    InvalidBoolean(u8),
    NonCanonical(&'static str),
    InvalidAuthority(&'static str),
    UnsupportedDialect(PackageDialect),
    ZeroSchema(&'static str),
    DescriptorMismatch {
        what: &'static str,
        declared: u8,
        actual: u8,
    },
    Integrity {
        expected: Digest,
        actual: Digest,
    },
    Bytecode(ChunkError),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge {
                what,
                actual,
                limit,
            } => write!(f, "{what} size/count {actual} exceeds limit {limit}"),
            Self::InvalidMagic => f.write_str("invalid Blu package magic"),
            Self::UnsupportedFormat(version) => {
                write!(f, "unsupported Blu package format {version}")
            }
            Self::Truncated => f.write_str("truncated Blu package"),
            Self::TrailingBytes(count) => write!(f, "{count} trailing package bytes"),
            Self::LengthOverflow => f.write_str("package length overflows this platform"),
            Self::InvalidUtf8 => f.write_str("package name is not UTF-8"),
            Self::InvalidName(reason) => write!(f, "invalid package name: {reason}"),
            Self::UnknownTag { what, value } => write!(f, "unknown {what} tag {value}"),
            Self::InvalidBoolean(value) => write!(f, "invalid boolean tag {value}"),
            Self::NonCanonical(what) => write!(f, "{what} are not strictly sorted and unique"),
            Self::InvalidAuthority(reason) => write!(f, "invalid authority requirement: {reason}"),
            Self::UnsupportedDialect(dialect) => {
                write!(f, "{dialect:?} cannot use the V1 Luau bytecode payload")
            }
            Self::ZeroSchema(what) => write!(f, "{what} has an all-zero schema digest"),
            Self::DescriptorMismatch {
                what,
                declared,
                actual,
            } => write!(
                f,
                "declared {what} {declared} differs from payload {actual}"
            ),
            Self::Integrity { expected, actual } => {
                write!(
                    f,
                    "package digest {actual} differs from computed {expected}"
                )
            }
            Self::Bytecode(error) => write!(f, "invalid bytecode payload: {error}"),
        }
    }
}

impl std::error::Error for PackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bytecode(error) => Some(error),
            _ => None,
        }
    }
}

fn validate_manifest(manifest: &Manifest, limits: &PackageLimits) -> Result<(), PackageError> {
    validate_one_name(&manifest.package.name, limits)?;
    validate_names(&manifest.exports, limits)?;
    validate_names(&manifest.imports, limits)?;
    limit(
        "capabilities",
        manifest.authority.capabilities.len(),
        limits.max_capabilities,
    )?;
    limit("imports", manifest.imports.len(), limits.max_imports)?;
    limit("exports", manifest.exports.len(), limits.max_exports)?;
    match (
        manifest.authority.profile,
        manifest.authority.capabilities.is_empty(),
    ) {
        (AuthorityProfile::Pure | AuthorityProfile::Trusted, false) => {
            return Err(PackageError::InvalidAuthority(
                "pure and trusted profiles cannot carry confined capabilities",
            ));
        }
        (AuthorityProfile::Confined, true) => {
            return Err(PackageError::InvalidAuthority(
                "confined profile requires at least one capability",
            ));
        }
        _ => {}
    }
    ensure_strictly_sorted(&manifest.authority.capabilities, "capabilities")?;
    ensure_strictly_sorted(&manifest.imports, "imports")?;
    ensure_strictly_sorted(&manifest.exports, "exports")?;
    for capability in &manifest.authority.capabilities {
        validate_one_name(&capability.name, limits)?;
        limit(
            "capability scope bytes",
            capability.scope.len(),
            limits.max_scope_bytes,
        )?;
    }
    for import in &manifest.imports {
        if import.schema == Digest::ZERO {
            return Err(PackageError::ZeroSchema("import"));
        }
    }
    for export in &manifest.exports {
        if export.schema == Digest::ZERO {
            return Err(PackageError::ZeroSchema("export"));
        }
    }
    Ok(())
}

trait Names {
    fn visit_names(&self, visit: &mut dyn FnMut(&Name));
}

impl Names for Vec<Import> {
    fn visit_names(&self, visit: &mut dyn FnMut(&Name)) {
        for item in self {
            visit(&item.service.namespace);
            visit(&item.service.name);
        }
    }
}

impl Names for Vec<Export> {
    fn visit_names(&self, visit: &mut dyn FnMut(&Name)) {
        for item in self {
            visit(&item.service.namespace);
            visit(&item.service.name);
        }
    }
}

fn validate_names<T: Names>(items: &T, limits: &PackageLimits) -> Result<(), PackageError> {
    let mut error = None;
    items.visit_names(&mut |name| {
        if error.is_none() {
            error = validate_one_name(name, limits).err();
        }
    });
    error.map_or(Ok(()), Err)
}

fn validate_one_name(name: &Name, limits: &PackageLimits) -> Result<(), PackageError> {
    limit("name bytes", name.as_str().len(), limits.max_name_bytes)?;
    crate::model::validate_name(name.as_str()).map_err(PackageError::InvalidName)
}

fn ensure_strictly_sorted<T: Ord>(items: &[T], what: &'static str) -> Result<(), PackageError> {
    if items.windows(2).all(|pair| pair[0] < pair[1]) {
        Ok(())
    } else {
        Err(PackageError::NonCanonical(what))
    }
}

fn validate_descriptor(manifest: &Manifest, chunk: &Chunk) -> Result<(), PackageError> {
    match manifest.bytecode.format {
        // `blu_bytecode::load` is the validator for this payload format.
        BytecodeFormat::Luau => {}
    }
    if !matches!(manifest.dialect, PackageDialect::Blu | PackageDialect::Luau) {
        return Err(PackageError::UnsupportedDialect(manifest.dialect));
    }
    if manifest.bytecode.version != chunk.version {
        return Err(PackageError::DescriptorMismatch {
            what: "bytecode version",
            declared: manifest.bytecode.version,
            actual: chunk.version,
        });
    }
    if manifest.bytecode.typeinfo_version != chunk.typeinfo_version {
        return Err(PackageError::DescriptorMismatch {
            what: "typeinfo version",
            declared: manifest.bytecode.typeinfo_version,
            actual: chunk.typeinfo_version,
        });
    }
    Ok(())
}

fn encode_unsigned(manifest: &[u8], payload: &[u8]) -> Result<Vec<u8>, PackageError> {
    let manifest_len = u32::try_from(manifest.len()).map_err(|_| PackageError::LengthOverflow)?;
    let payload_len = u64::try_from(payload.len()).map_err(|_| PackageError::LengthOverflow)?;
    let capacity = HEADER_BYTES
        .checked_add(manifest.len())
        .and_then(|value| value.checked_add(payload.len()))
        .ok_or(PackageError::LengthOverflow)?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&PACKAGE_FORMAT_V1.to_le_bytes());
    bytes.extend_from_slice(&manifest_len.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(manifest);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>, PackageError> {
    let mut bytes = Vec::new();
    put_name(&mut bytes, &manifest.package.name)?;
    put_version(&mut bytes, manifest.package.version);
    bytes.push(manifest.dialect as u8);
    bytes.push(manifest.bytecode.format as u8);
    bytes.push(manifest.bytecode.version);
    bytes.push(manifest.bytecode.typeinfo_version);
    bytes.push(manifest.authority.profile as u8);
    put_count(&mut bytes, manifest.authority.capabilities.len())?;
    for capability in &manifest.authority.capabilities {
        put_name(&mut bytes, &capability.name)?;
        put_bytes(&mut bytes, &capability.scope)?;
    }
    put_count(&mut bytes, manifest.imports.len())?;
    for import in &manifest.imports {
        bytes.push(import.source as u8);
        put_service(&mut bytes, &import.service)?;
        put_version(&mut bytes, import.version);
        bytes.extend_from_slice(import.schema.as_bytes());
        bytes.push(u8::from(import.optional));
    }
    put_count(&mut bytes, manifest.exports.len())?;
    for export in &manifest.exports {
        put_service(&mut bytes, &export.service)?;
        put_version(&mut bytes, export.version);
        bytes.extend_from_slice(export.schema.as_bytes());
    }
    Ok(bytes)
}

fn decode_manifest(bytes: &[u8], limits: &PackageLimits) -> Result<Manifest, PackageError> {
    let mut reader = Reader::new(bytes);
    let package = PackageIdentity {
        name: reader.name(limits)?,
        version: reader.version()?,
    };
    let dialect = match reader.byte()? {
        1 => PackageDialect::Blu,
        2 => PackageDialect::Luau,
        3 => PackageDialect::Lua51,
        4 => PackageDialect::Lua52,
        5 => PackageDialect::Lua53,
        6 => PackageDialect::Lua54,
        7 => PackageDialect::Lua55,
        value => {
            return Err(PackageError::UnknownTag {
                what: "dialect",
                value,
            });
        }
    };
    let format = match reader.byte()? {
        1 => BytecodeFormat::Luau,
        value => {
            return Err(PackageError::UnknownTag {
                what: "bytecode format",
                value,
            });
        }
    };
    let bytecode = BytecodeDescriptor {
        format,
        version: reader.byte()?,
        typeinfo_version: reader.byte()?,
    };
    let profile = match reader.byte()? {
        1 => AuthorityProfile::Pure,
        2 => AuthorityProfile::Confined,
        3 => AuthorityProfile::Trusted,
        value => {
            return Err(PackageError::UnknownTag {
                what: "authority profile",
                value,
            });
        }
    };
    let capability_count = reader.count("capabilities", limits.max_capabilities)?;
    let mut capabilities = Vec::with_capacity(capability_count);
    for _ in 0..capability_count {
        capabilities.push(CapabilityRequirement {
            name: reader.name(limits)?,
            scope: reader.sized_bytes("capability scope bytes", limits.max_scope_bytes)?,
        });
    }
    let import_count = reader.count("imports", limits.max_imports)?;
    let mut imports = Vec::with_capacity(import_count);
    for _ in 0..import_count {
        let source = match reader.byte()? {
            1 => ImportSource::Host,
            2 => ImportSource::Component,
            value => {
                return Err(PackageError::UnknownTag {
                    what: "import source",
                    value,
                });
            }
        };
        imports.push(Import {
            source,
            service: reader.service(limits)?,
            version: reader.version()?,
            schema: reader.digest()?,
            optional: match reader.byte()? {
                0 => false,
                1 => true,
                value => return Err(PackageError::InvalidBoolean(value)),
            },
        });
    }
    let export_count = reader.count("exports", limits.max_exports)?;
    let mut exports = Vec::with_capacity(export_count);
    for _ in 0..export_count {
        exports.push(Export {
            service: reader.service(limits)?,
            version: reader.version()?,
            schema: reader.digest()?,
        });
    }
    if reader.remaining() != 0 {
        return Err(PackageError::TrailingBytes(reader.remaining()));
    }
    Ok(Manifest {
        package,
        dialect,
        bytecode,
        authority: AuthorityRequirement {
            profile,
            capabilities,
        },
        imports,
        exports,
    })
}

fn put_name(bytes: &mut Vec<u8>, name: &Name) -> Result<(), PackageError> {
    let raw = name.as_str().as_bytes();
    let len = u16::try_from(raw.len()).map_err(|_| PackageError::LengthOverflow)?;
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(raw);
    Ok(())
}

fn put_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), PackageError> {
    let len = u32::try_from(value.len()).map_err(|_| PackageError::LengthOverflow)?;
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

fn put_count(bytes: &mut Vec<u8>, value: usize) -> Result<(), PackageError> {
    let value = u32::try_from(value).map_err(|_| PackageError::LengthOverflow)?;
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_version(bytes: &mut Vec<u8>, version: Version) {
    bytes.extend_from_slice(&version.major.to_le_bytes());
    bytes.extend_from_slice(&version.minor.to_le_bytes());
    bytes.extend_from_slice(&version.patch.to_le_bytes());
}

fn put_service(bytes: &mut Vec<u8>, service: &ServiceId) -> Result<(), PackageError> {
    put_name(bytes, &service.namespace)?;
    put_name(bytes, &service.name)
}

fn hash(bytes: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(bytes);
    Digest::new(hasher.finalize().into())
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn limit(what: &'static str, actual: usize, maximum: usize) -> Result<(), PackageError> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(PackageError::TooLarge {
            what,
            actual,
            limit: maximum,
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], PackageError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(PackageError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(PackageError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, PackageError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PackageError> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, PackageError> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, PackageError> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    fn usize_u32(&mut self) -> Result<usize, PackageError> {
        usize::try_from(self.u32()?).map_err(|_| PackageError::LengthOverflow)
    }

    fn usize_u64(&mut self) -> Result<usize, PackageError> {
        usize::try_from(self.u64()?).map_err(|_| PackageError::LengthOverflow)
    }

    fn count(&mut self, what: &'static str, maximum: usize) -> Result<usize, PackageError> {
        let count = self.usize_u32()?;
        limit(what, count, maximum)?;
        Ok(count)
    }

    fn sized_bytes(&mut self, what: &'static str, maximum: usize) -> Result<Vec<u8>, PackageError> {
        let len = self.usize_u32()?;
        limit(what, len, maximum)?;
        Ok(self.bytes(len)?.to_vec())
    }

    fn name(&mut self, limits: &PackageLimits) -> Result<Name, PackageError> {
        let len = usize::from(self.u16()?);
        limit("name bytes", len, limits.max_name_bytes)?;
        let value = std::str::from_utf8(self.bytes(len)?).map_err(|_| PackageError::InvalidUtf8)?;
        crate::model::validate_name(value).map_err(PackageError::InvalidName)?;
        Ok(Name::from_validated(value.to_owned()))
    }

    fn version(&mut self) -> Result<Version, PackageError> {
        Ok(Version::new(self.u32()?, self.u32()?, self.u32()?))
    }

    fn service(&mut self, limits: &PackageLimits) -> Result<ServiceId, PackageError> {
        Ok(ServiceId {
            namespace: self.name(limits)?,
            name: self.name(limits)?,
        })
    }

    fn digest(&mut self) -> Result<Digest, PackageError> {
        let mut digest = [0; 32];
        digest.copy_from_slice(self.bytes(32)?);
        Ok(Digest::new(digest))
    }
}
