//! Install use case.

use std::path::PathBuf;

use xpm_aur_core::plan::{AurBuild, InstallPlan};
use xpm_aur_core::resolver::{ResolveOptions, Resolver};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::scheduler::{self, BuiltBase};

/// Default build concurrency: 2, reduced on machines with fewer CPUs.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().min(2))
        .unwrap_or(2)
}

/// User-facing options for resolving and installing.
#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub needed: bool,
    pub force_aur: bool,
    pub force_repo: bool,
    pub rebuild: bool,
    pub remove_conflicts: bool,
    pub include_check_depends: bool,
    pub print_only: bool,
    pub target_asdeps: bool,
    /// Build AUR packages inside a clean chroot.
    pub chroot: bool,
    /// Maximum number of AUR package bases built concurrently.
    pub jobs: usize,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            needed: true,
            force_aur: false,
            force_repo: false,
            rebuild: false,
            remove_conflicts: false,
            include_check_depends: true,
            print_only: false,
            target_asdeps: false,
            chroot: false,
            jobs: default_jobs(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct InstallOutcome {
    pub official: usize,
    pub aur_bases: usize,
    pub aur_packages: usize,
    pub removals: usize,
    pub plan: InstallPlan,
}

impl App {
    /// Resolve, build and install AUR/official packages.
    pub fn install(
        &self,
        targets: &[String],
        options: &InstallOptions,
    ) -> AppResult<InstallOutcome> {
        if targets.is_empty() {
            return Ok(InstallOutcome::default());
        }

        let transaction = crate::progress::PhaseProgress::new(["sync", "resolve", "build"]);
        transaction.print("sync", 0, 3, "official databases");

        // Refresh official databases through xpm before reading them. This is
        // the only safe way to avoid resolving against a stale sync DB.
        self.xpm.sync()?;
        transaction.print("resolve", 1, 3, "dependency plan");

        let resolver = Resolver::new(
            &self.catalog,
            ResolveOptions {
                include_make_depends: true,
                include_check_depends: options.include_check_depends,
                force_aur: options.force_aur,
                force_repo: options.force_repo,
                rebuild: options.rebuild,
                remove_conflicts: options.remove_conflicts,
                target_asdeps: options.target_asdeps,
                chroot: options.chroot,
            },
        );
        let plan = resolver.resolve(targets)?;
        self.print_plan(&plan);

        if plan.is_empty() {
            return Ok(InstallOutcome {
                plan,
                ..InstallOutcome::default()
            });
        }
        if options.print_only {
            return Ok(InstallOutcome {
                official: plan.official.len(),
                aur_bases: plan.aur.len(),
                aur_packages: plan.aur.iter().map(|build| build.packages.len()).sum(),
                removals: plan.removals.len(),
                plan,
            });
        }

        crate::app::confirm("Proceed with the transaction?", self.no_confirm)?;

        // The user already approved this plan; never ask again inside xpm.
        let xpm = self.xpm.with_no_confirm(true);
        if options.chroot {
            // Authenticate sudo once before several parallel builds start.
            xpm.authenticate()?;
        }

        if !plan.removals.is_empty() {
            let names: Vec<String> = plan.removals.iter().map(|item| item.name.clone()).collect();
            xpm.remove(&names, false, false)?;
        }

        // In chroot mode make/check-only official dependencies are installed
        // inside each disposable root by xpm build, not on the host.
        let include_build_only = !options.chroot;
        let official_deps: Vec<String> = plan
            .official
            .iter()
            .filter(|action| action.asdeps && (include_build_only || !action.build_only))
            .map(|action| action.name.clone())
            .collect();
        let official_explicit: Vec<String> = plan
            .official
            .iter()
            .filter(|action| !action.asdeps)
            .map(|action| action.name.clone())
            .collect();
        xpm.install_official(&official_deps, true, options.needed)?;
        xpm.install_official(&official_explicit, false, options.needed)?;

        let aur_bases = plan.aur.len();
        let aur_packages = plan.aur.iter().map(|build| build.packages.len()).sum();
        transaction.print("build", 2, 3, "AUR package bases");
        let progress =
            crate::progress::PhaseProgress::new(plan.aur.iter().map(|build| build.base.as_str()));
        scheduler::run(
            &plan.aur,
            options.jobs,
            |build, preinstall| self.build_base(build, preinstall, &progress),
            |build, built| self.install_base(&xpm, build, built, &progress),
        )?;

        Ok(InstallOutcome {
            official: plan.official.len(),
            aur_bases,
            aur_packages,
            removals: plan.removals.len(),
            plan,
        })
    }

    /// Clone and build one base without touching xpm's package database.
    fn build_base(
        &self,
        build: &AurBuild,
        preinstall: &[PathBuf],
        progress: &crate::progress::PhaseProgress,
    ) -> AppResult<BuiltBase> {
        let checkout = self.paths.build_root.join(&build.base);
        progress.print(&build.base, 0, 3, "checkout");
        tracing::info!(base = %build.base, "cloning/updating AUR package base");
        let commit = self.git.checkout(&build.base, &checkout)?;

        let srcinfo = self.git.read_srcinfo(&checkout)?;
        for package in &build.packages {
            if !srcinfo.contains(&package.name) {
                return Err(AppError::MissingArtifact {
                    base: build.base.clone(),
                });
            }
        }

        progress.print(&build.base, 1, 3, "building");
        let artifacts =
            self.builder
                .build(&checkout, &build.base, preinstall, &build.build_deps)?;
        Ok(BuiltBase { commit, artifacts })
    }

    /// Install one finished base and update the sidecar. Always runs serially.
    fn install_base(
        &self,
        xpm: &xpm_aur_system::XpmClient,
        build: &AurBuild,
        built: &BuiltBase,
        progress: &crate::progress::PhaseProgress,
    ) -> AppResult<()> {
        let (dependency_files, explicit_files) = self.partition_artifacts(build, built)?;

        // Install split dependencies before explicit split packages so the
        // explicit package's dependency check can see them.
        progress.print(&build.base, 2, 3, "installing");
        xpm.install_files(&dependency_files, true)?;
        xpm.install_files(&explicit_files, false)?;

        let version = build
            .packages
            .first()
            .map(|package| package.version.to_string())
            .unwrap_or_default();
        let packages: Vec<String> = build
            .packages
            .iter()
            .map(|package| package.name.clone())
            .collect();
        let explicit = build.packages.iter().any(|package| package.explicit);
        self.state.update(|state| {
            state.packages.insert(
                build.base.clone(),
                xpm_aur_system::AurRecord::new(
                    &build.base,
                    version,
                    &built.commit,
                    packages,
                    explicit,
                ),
            );
        })?;

        progress.print(&build.base, 3, 3, "done");
        tracing::info!(base = %build.base, commit = %built.commit, "AUR package base installed");
        Ok(())
    }

    fn partition_artifacts(
        &self,
        build: &AurBuild,
        built: &BuiltBase,
    ) -> AppResult<(Vec<PathBuf>, Vec<PathBuf>)> {
        let mut dependency_files = Vec::new();
        let mut explicit_files = Vec::new();
        for package in &build.packages {
            let artifact =
                built
                    .artifacts
                    .get(&package.name)
                    .ok_or_else(|| AppError::MissingArtifact {
                        base: build.base.clone(),
                    })?;
            if package.explicit {
                explicit_files.push(artifact.path.clone());
            } else {
                dependency_files.push(artifact.path.clone());
            }
        }
        Ok((dependency_files, explicit_files))
    }

    pub fn print_plan(&self, plan: &InstallPlan) {
        if plan.removals.is_empty() && plan.official.is_empty() && plan.aur.is_empty() {
            println!("Nothing to do.");
            return;
        }

        if !plan.removals.is_empty() {
            println!("Remove:");
            for removal in &plan.removals {
                println!("  {} ({})", removal.name, removal.reason);
            }
        }
        if !plan.official.is_empty() {
            println!("Official:");
            for action in &plan.official {
                let reason = if action.asdeps {
                    if action.build_only {
                        "build dependency"
                    } else {
                        "dependency"
                    }
                } else {
                    "explicit"
                };
                println!("  {} ({reason})", action.name);
            }
        }
        if !plan.aur.is_empty() {
            println!("AUR:");
            for build in &plan.aur {
                let packages = build
                    .packages
                    .iter()
                    .map(|package| {
                        let reason = if package.explicit {
                            "explicit"
                        } else {
                            "dependency"
                        };
                        format!("{} {} ({reason})", package.name, package.version)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  {packages}");
            }
        }
    }
}
