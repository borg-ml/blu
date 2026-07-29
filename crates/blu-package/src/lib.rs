#![forbid(unsafe_code)]

//! Portable, integrity-checked envelopes for validated Blu bytecode.
//!
//! Decoding is inspection-only: a package declares authority requirements but
//! never grants authority or executes its payload.

mod codec;
mod model;

pub use codec::{Package, PackageError, PackageLimits};
pub use model::{
    AuthorityProfile, AuthorityRequirement, BytecodeDescriptor, BytecodeFormat,
    CapabilityRequirement, Digest, Export, Import, ImportSource, Manifest, Name, PackageDialect,
    PackageIdentity, ServiceId, Version,
};

/// The only package-envelope version accepted by this crate.
pub const PACKAGE_FORMAT_V1: u16 = 1;
