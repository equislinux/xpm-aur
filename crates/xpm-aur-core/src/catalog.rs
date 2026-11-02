//! The package catalog port used by the resolver.

use serde::{Deserialize, Serialize};

use crate::dependency::Dependency;
use crate::error::CoreResult;
use crate::package::PackageMeta;

/// Why a package is installed. Mirrors pacman's `%REASON%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InstallReason {
    Explicit,
    Dependency,
}

/// Where a candidate comes from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CandidateSource {
    /// Already in the local database.
    Installed,
    /// A binary package from a pacman/xpm sync repository.
    Official { repo: String, filename: String },
    /// A source package from the AUR.
    Aur,
}

/// A package and the information needed to decide whether to use it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub meta: PackageMeta,
    pub source: CandidateSource,
    pub reason: Option<InstallReason>,
    /// AUR votes, used only to break ties between providers.
    pub votes: u64,
    /// AUR popularity, used only to break ties between providers.
    pub popularity: u64,
    pub out_of_date: bool,
}

impl Candidate {
    pub fn new(meta: PackageMeta, source: CandidateSource) -> Self {
        Self {
            meta,
            source,
            reason: None,
            votes: 0,
            popularity: 0,
            out_of_date: false,
        }
    }

    pub fn satisfies(&self, dependency: &Dependency) -> bool {
        self.meta.satisfies(dependency)
    }

    pub fn is_installed(&self) -> bool {
        matches!(self.source, CandidateSource::Installed)
    }

    pub fn is_aur(&self) -> bool {
        matches!(self.source, CandidateSource::Aur)
    }
}

/// Everything the resolver needs to know about the host.
///
/// Implementations are allowed to perform I/O; the resolver itself is pure.
pub trait PackageCatalog {
    /// All installed packages, including reason and provides.
    fn installed(&self) -> CoreResult<Vec<Candidate>>;

    /// Official repository candidates matching the dependency name or a
    /// provided virtual name. The implementation should return exact-name
    /// matches before providers and respect repository priority.
    fn official_candidates(&self, dependency: &Dependency) -> CoreResult<Vec<Candidate>>;

    /// AUR candidates matching the dependency name or a provided virtual name.
    /// Exact-name matches must come before providers.
    fn aur_candidates(&self, dependency: &Dependency) -> CoreResult<Vec<Candidate>>;
}
