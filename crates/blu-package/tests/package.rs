use blu_package::{
    AuthorityProfile, AuthorityRequirement, BytecodeDescriptor, BytecodeFormat,
    CapabilityRequirement, Digest, Export, Import, ImportSource, Manifest, Name, Package,
    PackageDialect, PackageError, PackageIdentity, PackageLimits, ServiceId, Version,
};
use sha2::{Digest as _, Sha256};

// `return 1 + 2`, compiled by pinned luau-compile with default flags.
const RETURN_THREE_V12: &[u8] = &[
    0x0c, 0x03, 0x00, 0x00, 0x01, 0x23, 0x01, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x03, 0x41, 0x00, 0x00,
    0x00, 0x04, 0x00, 0x03, 0x00, 0x16, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x18, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn name(value: &str) -> Name {
    Name::new(value).unwrap()
}

fn service(namespace: &str, value: &str) -> ServiceId {
    ServiceId {
        namespace: name(namespace),
        name: name(value),
    }
}

fn schema(byte: u8) -> Digest {
    Digest::new([byte; 32])
}

fn manifest() -> Manifest {
    Manifest {
        package: PackageIdentity {
            name: name("example.plugin"),
            version: Version::new(1, 2, 3),
        },
        dialect: PackageDialect::Blu,
        bytecode: BytecodeDescriptor {
            format: BytecodeFormat::Luau,
            version: 12,
            typeinfo_version: 3,
        },
        authority: AuthorityRequirement {
            profile: AuthorityProfile::Pure,
            capabilities: Vec::new(),
        },
        imports: vec![Import {
            source: ImportSource::Host,
            service: service("blu.host", "log"),
            version: Version::new(1, 0, 0),
            schema: schema(1),
            optional: true,
        }],
        exports: vec![Export {
            service: service("example", "main"),
            version: Version::new(1, 0, 0),
            schema: schema(2),
        }],
    }
}

fn package() -> Package {
    Package::new(
        manifest(),
        RETURN_THREE_V12.to_vec(),
        PackageLimits::default(),
    )
    .unwrap()
}

#[test]
fn canonical_package_round_trips_with_stable_identity() {
    let package = package();
    let bytes = package.encode();
    let decoded = Package::decode(&bytes, PackageLimits::default()).unwrap();

    assert_eq!(decoded.manifest(), package.manifest());
    assert_eq!(decoded.payload(), RETURN_THREE_V12);
    assert_eq!(decoded.digest(), package.digest());
    assert_eq!(decoded.chunk().version, 12);
    assert_eq!(decoded.validated_chunk().as_chunk(), decoded.chunk());
    assert_eq!(decoded.encode(), bytes);
    assert_eq!(
        package.digest().to_string(),
        "9a4ee278f452814485bd4bb4b96249faec7a8567493d90f8ef0ef52f8373384a"
    );
}

#[test]
fn rejects_every_truncation_and_trailing_data() {
    let bytes = package().encode();
    for end in 0..bytes.len() {
        assert!(Package::decode(&bytes[..end], PackageLimits::default()).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        Package::decode(&trailing, PackageLimits::default()).unwrap_err(),
        PackageError::TrailingBytes(1)
    );
}

#[test]
fn digest_covers_header_manifest_and_payload() {
    let original = package().encode();
    for offset in [8, 22, original.len() - 33, original.len() - 1] {
        let mut corrupted = original.clone();
        corrupted[offset] ^= 1;
        assert!(matches!(
            Package::decode(&corrupted, PackageLimits::default()),
            Err(PackageError::Integrity { .. })
                | Err(PackageError::UnsupportedFormat(_))
                | Err(PackageError::Truncated)
                | Err(PackageError::TrailingBytes(_))
        ));
    }
}

#[test]
fn enforces_limits_before_nested_allocation() {
    let bytes = package().encode();
    let limits = PackageLimits {
        max_manifest_bytes: 8,
        ..PackageLimits::default()
    };
    assert!(matches!(
        Package::decode(&bytes, limits),
        Err(PackageError::TooLarge {
            what: "manifest bytes",
            ..
        })
    ));

    let limits = PackageLimits {
        max_payload_bytes: RETURN_THREE_V12.len() - 1,
        ..PackageLimits::default()
    };
    assert!(matches!(
        Package::decode(&bytes, limits),
        Err(PackageError::TooLarge {
            what: "payload bytes",
            ..
        })
    ));
}

#[test]
fn rejects_noncanonical_declarations_and_invalid_authority() {
    let mut unordered = manifest();
    unordered.imports = vec![
        Import {
            source: ImportSource::Host,
            service: service("z", "service"),
            version: Version::new(1, 0, 0),
            schema: schema(3),
            optional: false,
        },
        Import {
            source: ImportSource::Host,
            service: service("a", "service"),
            version: Version::new(1, 0, 0),
            schema: schema(4),
            optional: false,
        },
    ];
    assert!(matches!(
        Package::new(
            unordered,
            RETURN_THREE_V12.to_vec(),
            PackageLimits::default()
        ),
        Err(PackageError::NonCanonical("imports"))
    ));

    let mut authority = manifest();
    authority.authority = AuthorityRequirement {
        profile: AuthorityProfile::Pure,
        capabilities: vec![CapabilityRequirement {
            name: name("fs.read"),
            scope: b"workspace".to_vec(),
        }],
    };
    assert!(matches!(
        Package::new(
            authority,
            RETURN_THREE_V12.to_vec(),
            PackageLimits::default()
        ),
        Err(PackageError::InvalidAuthority(_))
    ));
}

#[test]
fn rejects_payload_descriptor_mismatch() {
    let mut mismatched = manifest();
    mismatched.bytecode.version = 11;
    assert_eq!(
        Package::new(
            mismatched,
            RETURN_THREE_V12.to_vec(),
            PackageLimits::default()
        )
        .unwrap_err(),
        PackageError::DescriptorMismatch {
            what: "bytecode version",
            declared: 11,
            actual: 12,
        }
    );
}

#[test]
fn v1_rejects_dialects_without_a_luau_bytecode_execution_profile() {
    for dialect in [
        PackageDialect::Lua51,
        PackageDialect::Lua52,
        PackageDialect::Lua53,
        PackageDialect::Lua54,
        PackageDialect::Lua55,
    ] {
        let mut unsupported = manifest();
        unsupported.dialect = dialect;
        assert_eq!(
            Package::new(
                unsupported,
                RETURN_THREE_V12.to_vec(),
                PackageLimits::default()
            )
            .unwrap_err(),
            PackageError::UnsupportedDialect(dialect)
        );
    }
}

#[test]
fn rejects_unknown_tags_after_integrity_verification() {
    let mut bytes = package().encode();
    // Header, name length/name, and package version precede the dialect byte.
    let dialect_offset = 22 + 2 + "example.plugin".len() + 12;
    bytes[dialect_offset] = 255;
    resign(&mut bytes);
    assert_eq!(
        Package::decode(&bytes, PackageLimits::default()).unwrap_err(),
        PackageError::UnknownTag {
            what: "dialect",
            value: 255,
        }
    );
}

#[test]
fn names_use_a_narrow_portable_grammar() {
    for invalid in ["", "Upper", ".hidden", "trailing.", "has/slash", "naïve"] {
        assert!(Name::new(invalid).is_err(), "{invalid:?} was accepted");
    }
    assert_eq!(name("blu.host-v1").as_str(), "blu.host-v1");
}

fn resign(bytes: &mut [u8]) {
    let digest_start = bytes.len() - 32;
    let mut hasher = Sha256::new();
    hasher.update(b"blu.package.v1\0");
    hasher.update(&bytes[..digest_start]);
    let digest: [u8; 32] = hasher.finalize().into();
    bytes[digest_start..].copy_from_slice(&digest);
}
