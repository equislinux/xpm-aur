//! Search, info, remove, query, upgrade, adopt and clean use cases.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime};

use xpm_aur_aur::{AurPackage, SearchBy};
use xpm_aur_core::catalog::{InstallReason, PackageCatalog};
use xpm_aur_core::dependency::Dependency;
use xpm_aur_core::version::Evr;
use xpm_aur_system::{AurRecord, State};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::install::{InstallOptions, InstallOutcome};

#[derive(Clone, Debug, Default)]
pub struct RemoveOptions {
    pub recursive: bool,
    pub nosave: bool,
}

#[derive(Clone, Debug, Default)]
pub struct UpgradeOutcome {
    pub official_upgraded: bool,
    pub aur_updates: Vec<String>,
    pub install: Option<InstallOutcome>,
}

impl App {
    pub fn sync(&self) -> AppResult<()> {
        self.xpm.sync()?;
        Ok(())
    }

    pub fn search(&self, query: &str, by: SearchBy) -> AppResult<Vec<AurPackage>> {
        Ok(self.catalog.aur_search(query, by)?)
    }

    pub fn info(&self, names: &[String]) -> AppResult<Vec<AurPackage>> {
        Ok(self.catalog.aur_info(names)?)
    }

    pub fn query(&self) -> AppResult<Vec<AurRecord>> {
        let state = self.state.load()?;
        Ok(state.packages.into_values().collect())
    }

    /// Remove packages through xpm and reconcile the AUR sidecar afterwards.
    pub fn remove(&self, names: &[String], options: &RemoveOptions) -> AppResult<()> {
        if names.is_empty() {
            return Ok(());
        }
        crate::app::confirm("Remove the selected packages?", self.no_confirm)?;
        self.xpm.remove(names, options.recursive, options.nosave)?;
        self.reconcile_state()?;
        Ok(())
    }

    /// Upgrade official packages with xpm, then rebuild AUR packages whose AUR
    /// version changed (and, with `--devel`, whose git HEAD changed).
    pub fn upgrade(
        &self,
        devel: bool,
        only_aur: bool,
        install: &InstallOptions,
    ) -> AppResult<UpgradeOutcome> {
        let mut outcome = UpgradeOutcome::default();

        if !only_aur {
            self.xpm.sync()?;
            self.xpm.upgrade()?;
            outcome.official_upgraded = true;
        }

        let state = self.state.load()?;
        if state.packages.is_empty() {
            return Ok(outcome);
        }

        let probe_names: Vec<String> = state
            .packages
            .values()
            .filter_map(|record| record.packages.first().cloned())
            .collect();
        let infos = self.catalog.aur_info(&probe_names)?;
        let by_name: BTreeMap<String, AurPackage> = infos
            .into_iter()
            .map(|info| (info.name.clone(), info))
            .collect();

        let mut targets = Vec::new();
        for (base, record) in &state.packages {
            let Some(probe) = record.packages.first() else {
                continue;
            };
            let info = by_name.get(probe);
            let version_changed = match info {
                Some(info) => version_is_newer(&info.version, &record.version),
                None => {
                    tracing::warn!(base = %base, "AUR package disappeared; leaving it installed");
                    false
                }
            };
            let commit_changed = devel
                && is_vcs(base)
                && match self.git.remote_head(base) {
                    Ok(head) => head != record.commit,
                    Err(err) => {
                        tracing::warn!(base = %base, error = %err, "could not check VCS HEAD");
                        false
                    }
                };

            if version_changed || commit_changed {
                targets.extend(record.packages.iter().cloned());
                outcome.aur_updates.push(base.clone());
            }
        }

        if targets.is_empty() {
            return Ok(outcome);
        }

        let mut options = install.clone();
        options.force_aur = true;
        options.rebuild = true;
        options.needed = false;
        outcome.install = Some(self.install(&targets, &options)?);
        Ok(outcome)
    }

    /// Adopt AUR packages that were installed by another helper (usually yay)
    /// into the sidecar so future upgrades are aware of them.
    pub fn adopt(&self) -> AppResult<usize> {
        let installed = self.catalog.installed()?;
        let official: HashSet<String> = self
            .catalog
            .database()
            .sync_packages()?
            .into_iter()
            .map(|candidate| candidate.meta.name)
            .collect();
        let foreign: Vec<_> = installed
            .iter()
            .filter(|candidate| !official.contains(&candidate.meta.name))
            .cloned()
            .collect();
        if foreign.is_empty() {
            return Ok(0);
        }

        let names: Vec<String> = foreign
            .iter()
            .map(|candidate| candidate.meta.name.clone())
            .collect();
        let infos = self.catalog.aur_info(&names)?;
        let by_name: BTreeMap<String, AurPackage> = infos
            .into_iter()
            .map(|info| (info.name.clone(), info))
            .collect();

        let mut adopted = 0;
        self.state.update(|state| {
            for candidate in &foreign {
                let Some(info) = by_name.get(&candidate.meta.name) else {
                    continue;
                };
                let explicit = candidate.reason == Some(InstallReason::Explicit);
                let record = state
                    .packages
                    .entry(info.package_base.clone())
                    .or_insert_with(|| AurRecord {
                        base: info.package_base.clone(),
                        version: candidate.meta.version.to_string(),
                        commit: String::new(),
                        installed_at: 0,
                        explicit,
                        packages: Vec::new(),
                    });
                if !record.packages.contains(&candidate.meta.name) {
                    record.packages.push(candidate.meta.name.clone());
                }
                record.explicit |= explicit;
                adopted += 1;
            }
        })?;

        Ok(adopted)
    }

    /// Remove cached build trees and package artifacts. With `all`, remove the
    /// whole cache; otherwise keep entries newer than `keep_days`.
    pub fn clean(&self, all: bool, keep_days: u64) -> AppResult<usize> {
        let mut removed = 0;
        for path in [
            &self.paths.build_root,
            &self.paths.package_root,
            &self.paths.sources_root,
        ] {
            if all {
                if path.exists() {
                    removed += remove_tree(path)?;
                }
            } else {
                removed += remove_old_entries(path, keep_days)?;
            }
        }
        Ok(removed)
    }

    fn reconcile_state(&self) -> AppResult<()> {
        let installed: HashSet<String> = self
            .catalog
            .installed()?
            .into_iter()
            .map(|candidate| candidate.meta.name)
            .collect();
        self.state.update(|state| {
            state.packages.retain(|_, record| {
                record.packages.retain(|name| installed.contains(name));
                !record.packages.is_empty()
            });
        })?;
        Ok(())
    }
}

fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (Evr::parse(candidate), Evr::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => candidate != current,
    }
}

fn is_vcs(base: &str) -> bool {
    ["-git", "-svn", "-hg", "-bzr", "-cvs", "-nightly"]
        .iter()
        .any(|suffix| base.ends_with(suffix))
}

fn remove_tree(path: &Path) -> AppResult<usize> {
    if !path.exists() {
        return Ok(0);
    }
    std::fs::remove_dir_all(path).map_err(|source| AppError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(1)
}

fn remove_old_entries(path: &Path, keep_days: u64) -> AppResult<usize> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(AppError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(keep_days.saturating_mul(86_400)))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|source| AppError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let entry_path = entry.path();
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if modified < cutoff {
            if entry_path.is_dir() {
                std::fs::remove_dir_all(&entry_path).map_err(|source| AppError::Io {
                    path: entry_path.display().to_string(),
                    source,
                })?;
            } else {
                std::fs::remove_file(&entry_path).map_err(|source| AppError::Io {
                    path: entry_path.display().to_string(),
                    source,
                })?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

/// Used by the CLI to render state without importing system internals.
pub fn state_packages(state: &State) -> Vec<AurRecord> {
    state.packages.values().cloned().collect()
}

/// Convenience for `xpm-aur info` callers that want a single dependency check.
pub fn is_satisfied_by_installed(names: &[String], dependency: &Dependency) -> bool {
    names.iter().any(|name| name == &dependency.name)
}
