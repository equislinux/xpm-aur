//! Adapter for `xpm build`.
//!
//! The build command lives in xpm so every source build in the X ecosystem has
//! one implementation. xpm-aur only passes the checkout, output directory and
//! JSON result path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use xpm_aur_core::process::{CommandRunner, CommandSpec, StdioMode, run_checked};

use crate::error::{BuildError, BuildResult};

/// One built package file.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
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

/// Configuration for one `xpm build` invocation.
#[derive(Clone, Debug)]
pub struct BuildConfig {
    pub program: String,
    pub pkgdest: PathBuf,
    pub srcdest: Option<PathBuf>,
    pub builddir: Option<PathBuf>,
    pub run_checks: bool,
    /// Build in a clean pacstrap/systemd-nspawn chroot.
    pub chroot: bool,
    pub chroot_dir: Option<PathBuf>,
    pub extra_args: Vec<String>,
}

impl BuildConfig {
    pub fn new(pkgdest: impl Into<PathBuf>) -> Self {
        Self {
            program: "xpm".into(),
            pkgdest: pkgdest.into(),
            srcdest: None,
            builddir: None,
            run_checks: true,
            chroot: false,
            chroot_dir: None,
            extra_args: Vec::new(),
        }
    }
}

/// Runs `xpm build` in an AUR checkout.
pub struct XpmBuilder {
    runner: Arc<dyn CommandRunner>,
    config: BuildConfig,
}

#[derive(Deserialize)]
struct RawArtifacts {
    pkgbase: String,
    packages: Vec<BuiltPackage>,
}

impl XpmBuilder {
    pub fn new(runner: Arc<dyn CommandRunner>, config: BuildConfig) -> Self {
        Self { runner, config }
    }

    pub fn build(
        &self,
        checkout: &Path,
        package_base: &str,
        preinstall: &[PathBuf],
    ) -> BuildResult<BuildArtifacts> {
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

        let json_path = pkgdest.join(".xpm-build.json");
        let mut spec = CommandSpec::new(&self.config.program)
            .arg("build")
            .arg(checkout)
            .arg("--pkgdest")
            .arg(&pkgdest)
            .arg("--json")
            .arg(&json_path)
            .stdio(StdioMode::Inherit);

        if !self.config.run_checks {
            spec = spec.arg("--nocheck");
        }
        if self.config.chroot {
            spec = spec.arg("--chroot");
            if let Some(dir) = &self.config.chroot_dir {
                spec = spec.arg("--chroot-dir").arg(dir);
            }
        }
        if let Some(srcdest) = &self.config.srcdest {
            spec = spec.arg("--srcdest").arg(srcdest);
        }
        if let Some(builddir) = &self.config.builddir {
            spec = spec.arg("--builddir").arg(builddir);
        }
        for package in preinstall {
            spec = spec.arg("--preinstall").arg(package);
        }
        if !self.config.extra_args.is_empty() {
            spec = spec.arg("--");
            for arg in &self.config.extra_args {
                spec = spec.arg(arg);
            }
        }

        let output = run_checked(self.runner.as_ref(), &spec)?;
        let _ = output;

        let text = std::fs::read_to_string(&json_path).map_err(|source| BuildError::Io {
            path: json_path.display().to_string(),
            source,
        })?;
        let raw: RawArtifacts = serde_json::from_str(&text).map_err(|source| BuildError::Json {
            path: json_path.display().to_string(),
            source,
        })?;

        Ok(BuildArtifacts {
            base: raw.pkgbase,
            packages: raw.packages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use xpm_aur_core::process::{CommandOutput, ProcessError};

    #[derive(Default)]
    struct FakeXpm {
        calls: Mutex<Vec<CommandSpec>>,
    }

    impl CommandRunner for FakeXpm {
        fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, ProcessError> {
            self.calls.lock().unwrap().push(spec.clone());
            let json = spec
                .args
                .iter()
                .position(|arg| arg == "--json")
                .and_then(|index| spec.args.get(index + 1))
                .expect("--json path");
            let json = std::path::PathBuf::from(json);
            std::fs::write(
                json,
                r#"{"source_dir":"/tmp/demo","pkgbase":"demo","packages":[{"name":"demo","path":"/tmp/demo/demo-1.0-1-any.pkg.tar.zst"}]}"#,
            )
            .unwrap();
            Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn delegates_builds_to_xpm_and_parses_json() {
        let runner = Arc::new(FakeXpm::default());
        let pkgdest = std::env::temp_dir().join(format!(
            "xpm-aur-build-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let builder = XpmBuilder::new(runner.clone(), BuildConfig::new(&pkgdest));
        let checkout = std::env::temp_dir().join("xpm-aur-build-checkout");
        let artifacts = builder.build(&checkout, "demo", &[]).unwrap();
        assert_eq!(artifacts.base, "demo");
        assert_eq!(artifacts.packages[0].name, "demo");

        let calls = runner.calls.lock().unwrap();
        let args: Vec<String> = calls[0]
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "build");
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--pkgdest".to_string()));
        let _ = std::fs::remove_dir_all(pkgdest);
    }
}
