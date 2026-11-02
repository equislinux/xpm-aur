//! Errors produced by the pure domain layer.

use thiserror::Error;

/// A failure that can be explained without knowing which adapter is in use.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid dependency specification: {0}")]
    InvalidDependency(String),

    #[error("invalid version: {0}")]
    InvalidVersion(String),

    #[error("package not found: {0}")]
    PackageNotFound(String),

    #[error("no candidate satisfies {dependency} (required by {required_by})")]
    UnsatisfiedDependency {
        dependency: String,
        required_by: String,
    },

    #[error("dependency cycle between AUR package bases: {0}")]
    DependencyCycle(String),

    #[error("package {package} conflicts with {other}")]
    Conflict { package: String, other: String },

    #[error("package {package} replaces {other}; remove it first or pass --remove-conflicts")]
    Replaces { package: String, other: String },

    #[error("catalog error: {0}")]
    Catalog(String),

    #[error("an installed package is required by another package: {0}")]
    RequiredPackage(String),

    #[error("internal resolver invariant violated: {0}")]
    Invariant(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
