//! `makepkg` runner and artifact discovery.
//!
//! xpm-aur never invokes `makepkg --syncdeps` or `--install`; dependencies are
//! installed through xpm first and the build uses `--nodeps`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xpm_aur_core::process::{CommandRunner, CommandSpec, StdioMode, run_checked};

use crate::error::{BuildError, BuildResult};

/// One built package file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltPackage {
    pub name: String,
    pub path: PathBuf,
}

/// Artifacts produced by one package base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildArtifacts {
    pub base: String,
    pub packages: Vec<BuiltPackage>,
}

impl BuildArtifacts {
    pub fn get(&self, name: &str) -> Option<&BuiltPackage> {
        self.packages.iter().find(|package| package.name == name)
    }
}

/// Configuration for one makepkg invocation.
#[derive(Clone, Debug)]
pub struct BuildConfig {
    pub program: String,
    pub pkgdest: PathBuf,
    pub srcdest: Option<PathBuf>,
    pub builddir: Option<PathBuf>,
    pub run_checks: bool,
    pub extra_args: Vec<String>,
}

impl BuildConfig {
    pub fn new(pkgdest: impl Into<PathBuf>) -> Self {
        Self {
            program: "makepkg".into(),
            pkgdest: pkgdest.into(),
            srcdest: None,
            builddir: None,
            run_checks: true,
            extra_args: Vec::new(),
        }
    }
}

/// Runs makepkg in an AUR checkout.
pub struct MakepkgBuilder {
    runner: Arc<dyn CommandRunner>,
    config: BuildConfig,
}

impl MakepkgBuilder {
    pub fn new(runner: Arc<dyn CommandRunner>, config: BuildConfig) -> Self {
        Self { runner, config }
    }

    pub fn build(&self, checkout: &Path, package_base: &str) -> BuildResult<BuildArtifacts> {
        if is_effective_root() {
            return Err(BuildError::RunningAsRoot);
        }

        let pkgdest = self.config.pkgdest.join(package_base);
        if pkgdest.exists() {
            std::fs::remove_dir_all(&pkgdest).map_err(|source| BuildError::Io {
                path: pkgdest.display().to_string(),
                source,
            })?;
        }
        std::fs::create_dir_all(&pkgdest).map_err(|source| BuildError::Io {
            path: pkgdest.display().to_string(),
            source,
        })?;

        let mut spec = CommandSpec::new(&self.config.program)
            .arg("--nodeps")
            .arg("--noconfirm")
            .arg("--cleanbuild")
            .arg("--force")
            .arg("--noprogressbar")
            .cwd(checkout)
            .env("PKGDEST", &pkgdest)
            .env("PKGEXT", ".pkg.tar.zst")
            .stdio(StdioMode::Inherit);

        if !self.config.run_checks {
            spec = spec.arg("--nocheck");
        }
        if let Some(srcdest) = &self.config.srcdest {
            spec = spec.env("SRCDEST", srcdest).env("SRCPKGDEST", srcdest);
        }
        if let Some(builddir) = &self.config.builddir {
            spec = spec.env("BUILDDIR", builddir);
        }
        for arg in &self.config.extra_args {
            spec = spec.arg(arg);
        }

        let output = run_checked(self.runner.as_ref(), &spec)?;
        let _ = output;

        let mut packages = Vec::new();
        let entries = std::fs::read_dir(&pkgdest).map_err(|source| BuildError::Io {
            path: pkgdest.display().to_string(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| BuildError::Io {
                path: pkgdest.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !filename.ends_with(".pkg.tar.zst") {
                continue;
            }
            let name = package_name_from_filename(filename)?;
            packages.push(BuiltPackage { name, path });
        }

        if packages.is_empty() {
            return Err(BuildError::NoArtifacts(pkgdest.display().to_string()));
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(BuildArtifacts {
            base: package_base.to_string(),
            packages,
        })
    }
}

/// Extract `name` from `name-pkgver-pkgrel-arch.pkg.tar.zst`.
pub fn package_name_from_filename(filename: &str) -> BuildResult<String> {
    let Some(stem) = filename.strip_suffix(".pkg.tar.zst") else {
        return Err(BuildError::InvalidPackageFilename(filename.into()));
    };
    let mut parts = stem.rsplitn(4, '-');
    let _arch = parts.next();
    let _pkgrel = parts.next();
    let _pkgver = parts.next();
    let name = parts.next();
    match name {
        Some(name) if !name.is_empty() => Ok(name.to_string()),
        _ => Err(BuildError::InvalidPackageFilename(filename.into())),
    }
}

fn is_effective_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid() == 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_names_with_hyphens() {
        assert_eq!(
            package_name_from_filename("foo-bar-1.2.3-4-x86_64.pkg.tar.zst").unwrap(),
            "foo-bar"
        );
        assert_eq!(
            package_name_from_filename("foo-1:2.0-1-any.pkg.tar.zst").unwrap(),
            "foo"
        );
        assert!(package_name_from_filename("invalid.tar.zst").is_err());
    }
}
