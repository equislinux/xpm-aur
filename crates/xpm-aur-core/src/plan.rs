//! The immutable result of dependency resolution.

use serde::{Deserialize, Serialize};

use crate::version::Evr;

/// One official package to install or upgrade with xpm.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OfficialAction {
    pub name: String,
    pub asdeps: bool,
    /// Requested only as a make/check dependency of an AUR build. In chroot
    /// mode it is installed inside the disposable root instead of the host.
    #[serde(default)]
    pub build_only: bool,
}

/// One split package produced by an AUR package base.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AurPackagePlan {
    pub name: String,
    pub version: Evr,
    /// True when the user named this package explicitly.
    pub explicit: bool,
}

/// One AUR package base to clone, build and install.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AurBuild {
    pub base: String,
    pub packages: Vec<AurPackagePlan>,
    /// Other AUR bases whose artifacts must be built and installed first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Official packages this base needs inside its clean chroot.
    #[serde(default)]
    pub build_deps: Vec<String>,
}

/// A package that must be removed before the transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemovalAction {
    pub name: String,
    pub reason: String,
}

/// Everything xpm-aur will do, in a deterministic order.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallPlan {
    /// Official packages, installed before any build starts.
    pub official: Vec<OfficialAction>,
    /// AUR package bases in build order (dependencies first).
    pub aur: Vec<AurBuild>,
    /// Installed packages that conflict with or are replaced by the plan.
    pub removals: Vec<RemovalAction>,
}

impl InstallPlan {
    pub fn is_empty(&self) -> bool {
        self.official.is_empty() && self.aur.is_empty() && self.removals.is_empty()
    }

    pub fn aur_package_names(&self) -> impl Iterator<Item = &str> {
        self.aur
            .iter()
            .flat_map(|build| build.packages.iter().map(|package| package.name.as_str()))
    }
}
