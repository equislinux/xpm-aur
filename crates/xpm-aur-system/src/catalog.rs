//! Combines pacman repositories and AUR RPC into the resolver catalog port.

use xpm_aur_aur::{AurRpc, SearchBy};
use xpm_aur_core::catalog::{Candidate, PackageCatalog};
use xpm_aur_core::dependency::Dependency;
use xpm_aur_core::error::{CoreError, CoreResult};

use crate::pacman_db::PacmanDatabase;

/// Catalog backed by the real host: pacman DBs plus AUR RPC.
pub struct SystemCatalog {
    db: PacmanDatabase,
    rpc: AurRpc,
}

impl SystemCatalog {
    pub fn new(db: PacmanDatabase, rpc: AurRpc) -> Self {
        Self { db, rpc }
    }

    pub fn database(&self) -> &PacmanDatabase {
        &self.db
    }

    /// Search AUR using the public `name-desc` mode. Used by `xpm-aur search`.
    pub fn aur_search(
        &self,
        query: &str,
        by: SearchBy,
    ) -> Result<Vec<xpm_aur_aur::AurPackage>, CoreError> {
        self.rpc
            .search(query, by)
            .map_err(|err| CoreError::Catalog(err.to_string()))
    }

    /// Fetch AUR info for exact package names.
    pub fn aur_info(&self, names: &[String]) -> Result<Vec<xpm_aur_aur::AurPackage>, CoreError> {
        self.rpc
            .info(names)
            .map_err(|err| CoreError::Catalog(err.to_string()))
    }
}

impl PackageCatalog for SystemCatalog {
    fn installed(&self) -> CoreResult<Vec<Candidate>> {
        self.db.installed().map_err(Into::into)
    }

    fn official_candidates(&self, dependency: &Dependency) -> CoreResult<Vec<Candidate>> {
        self.db.sync_candidates(dependency).map_err(Into::into)
    }

    fn aur_candidates(&self, dependency: &Dependency) -> CoreResult<Vec<Candidate>> {
        let exact = self
            .rpc
            .info(std::slice::from_ref(&dependency.name))
            .map_err(|err| CoreError::Catalog(err.to_string()))?;

        let mut candidates: Vec<Candidate> = exact
            .into_iter()
            .map(|package| package.into_candidate())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Catalog(err.to_string()))?;

        // If the exact package exists but none of its versions satisfy the
        // constraint, a virtual provider may still satisfy it.
        let exact_satisfies = candidates
            .iter()
            .any(|candidate| candidate.satisfies(dependency));
        if !exact_satisfies {
            let providers = self
                .rpc
                .providers(&dependency.name)
                .map_err(|err| CoreError::Catalog(err.to_string()))?;
            candidates.extend(
                providers
                    .into_iter()
                    .map(|package| package.into_candidate())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| CoreError::Catalog(err.to_string()))?,
            );
        }

        // Deduplicate by package name; AUR search can return a package through
        // both an exact lookup and a provides lookup.
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|candidate| seen.insert(candidate.meta.name.clone()));
        candidates.sort_by(|a, b| {
            let a_exact = a.meta.name == dependency.name;
            let b_exact = b.meta.name == dependency.name;
            b_exact
                .cmp(&a_exact)
                .then_with(|| b.votes.cmp(&a.votes))
                .then_with(|| b.popularity.cmp(&a.popularity))
                .then_with(|| a.meta.name.cmp(&b.meta.name))
        });
        Ok(candidates)
    }
}
