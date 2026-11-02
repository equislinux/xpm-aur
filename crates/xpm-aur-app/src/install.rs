//! Install use case.

use xpm_aur_core::plan::InstallPlan;
use xpm_aur_core::resolver::{ResolveOptions, Resolver};

use crate::app::App;
use crate::error::{AppError, AppResult};

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

        // Refresh official databases through xpm before reading them. This is
        // the only safe way to avoid resolving against a stale sync DB.
        self.xpm.sync()?;

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

        if !plan.removals.is_empty() {
            let names: Vec<String> = plan.removals.iter().map(|item| item.name.clone()).collect();
            self.xpm.remove(&names, false, false)?;
        }

        let official_deps: Vec<String> = plan
            .official
            .iter()
            .filter(|action| action.asdeps)
            .map(|action| action.name.clone())
            .collect();
        let official_explicit: Vec<String> = plan
            .official
            .iter()
            .filter(|action| !action.asdeps)
            .map(|action| action.name.clone())
            .collect();
        self.xpm
            .install_official(&official_deps, true, options.needed)?;
        self.xpm
            .install_official(&official_explicit, false, options.needed)?;

        let mut aur_bases = 0;
        let mut aur_packages = 0;
        for build in &plan.aur {
            self.build_and_install(build, &plan)?;
            aur_bases += 1;
            aur_packages += build.packages.len();
        }

        Ok(InstallOutcome {
            official: plan.official.len(),
            aur_bases,
            aur_packages,
            removals: plan.removals.len(),
            plan,
        })
    }

    fn build_and_install(
        &self,
        build: &xpm_aur_core::plan::AurBuild,
        plan: &InstallPlan,
    ) -> AppResult<()> {
        let checkout = self.paths.build_root.join(&build.base);
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

        let artifacts = self.builder.build(&checkout, &build.base)?;
        let mut dependency_files = Vec::new();
        let mut explicit_files = Vec::new();
        for package in &build.packages {
            let artifact =
                artifacts
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

        // Install split dependencies before explicit split packages so the
        // explicit package's dependency check can see them.
        self.xpm.install_files(&dependency_files, true)?;
        self.xpm.install_files(&explicit_files, false)?;

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
                xpm_aur_system::AurRecord::new(&build.base, version, &commit, packages, explicit),
            );
        })?;

        // Keep the plan argument meaningful for future hooks (e.g. install
        // reason reconciliation). It also documents that the build belongs to
        // the resolved transaction.
        let _ = plan;
        tracing::info!(base = %build.base, %commit, "AUR package base installed");
        Ok(())
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
                    "dependency"
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
