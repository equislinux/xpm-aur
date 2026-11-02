//! Pure domain layer for `xpm-aur`.
//!
//! This crate deliberately contains no filesystem, network or process access.
//! Adapters live in `xpm-aur-aur`, `xpm-aur-system` and `xpm-aur-build`.
//! The resolver only talks to the [`PackageCatalog`] port, which makes the
//! dependency algorithm deterministic and easy to test with fakes.

pub mod catalog;
pub mod dependency;
pub mod error;
pub mod package;
pub mod plan;
pub mod process;
pub mod resolver;
pub mod version;

pub use catalog::{Candidate, CandidateSource, InstallReason, PackageCatalog};
pub use dependency::Dependency;
pub use error::CoreError;
pub use package::PackageMeta;
pub use plan::{AurBuild, AurPackagePlan, InstallPlan, OfficialAction, RemovalAction};
pub use resolver::{ResolveOptions, Resolver};
pub use version::{Evr, VersionConstraint, VersionOperator, vercmp};
