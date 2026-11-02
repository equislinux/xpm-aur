//! Package metadata shared by official, installed and AUR candidates.

use serde::{Deserialize, Serialize};

use crate::dependency::Dependency;
use crate::version::Evr;

/// Metadata needed to resolve and describe a package.
///
/// The same type is used for official repositories, installed packages and AUR
/// entries so the resolver never has to special-case a source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageMeta {
    pub name: String,
    /// AUR package base and split-package parent. Equal to `name` for normal
    /// packages and official packages.
    pub base: String,
    pub version: Evr,
    pub description: Option<String>,
    pub url: Option<String>,
    pub depends: Vec<Dependency>,
    pub make_depends: Vec<Dependency>,
    pub check_depends: Vec<Dependency>,
    pub opt_depends: Vec<Dependency>,
    pub provides: Vec<Dependency>,
    pub conflicts: Vec<Dependency>,
    pub replaces: Vec<Dependency>,
}

impl PackageMeta {
    /// Minimal constructor for tests and adapters.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            base: name.clone(),
            name,
            version: Evr::parse(&version.into()).expect("valid test version"),
            description: None,
            url: None,
            depends: Vec::new(),
            make_depends: Vec::new(),
            check_depends: Vec::new(),
            opt_depends: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
            replaces: Vec::new(),
        }
    }

    /// True when this package version or one of its `provides` entries
    /// satisfies `dependency`.
    pub fn satisfies(&self, dependency: &Dependency) -> bool {
        if self.name == dependency.name {
            match &dependency.constraint {
                None => return true,
                Some(constraint) => return self.version.matches(constraint),
            }
        }

        self.provides.iter().any(|provided| {
            if provided.name != dependency.name {
                return false;
            }
            match (&dependency.constraint, &provided.constraint) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(required), Some(available)) => available.version.matches(required),
            }
        })
    }

    /// Names this package can satisfy, including its own name.
    pub fn satisfies_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str())
            .chain(self.provides.iter().map(|provide| provide.name.as_str()))
    }
}
