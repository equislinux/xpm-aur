//! AUR git checkout management.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xpm_aur_core::process::{CommandRunner, CommandSpec, run_checked};

use crate::error::{AurError, AurResult};
use crate::srcinfo::{SrcInfo, read_srcinfo};

/// Clone and update AUR package bases.
pub struct AurGit {
    base_url: String,
    runner: Arc<dyn CommandRunner>,
}

impl AurGit {
    pub fn new(base_url: impl Into<String>, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            runner,
        }
    }

    pub fn url(&self, package_base: &str) -> String {
        format!("{}/{package_base}.git", self.base_url)
    }

    /// Clone or fast-forward `package_base` into `dir` and return HEAD.
    pub fn checkout(&self, package_base: &str, dir: &Path) -> AurResult<String> {
        if dir.join(".git").is_dir() {
            self.run_git(dir, &["fetch", "--depth=1", "--quiet", "origin", "HEAD"])?;
            self.run_git(dir, &["reset", "--hard", "--quiet", "FETCH_HEAD"])?;
        } else {
            if dir.exists() {
                let mut entries = std::fs::read_dir(dir).map_err(|source| AurError::Io {
                    path: dir.display().to_string(),
                    source,
                })?;
                if entries.next().is_some() {
                    return Err(AurError::SrcInfo(format!(
                        "build directory {} exists but is not an AUR checkout",
                        dir.display()
                    )));
                }
            }
            let url = self.url(package_base);
            let output = run_checked(
                self.runner.as_ref(),
                &CommandSpec::new("git")
                    .args(["clone", "--depth=1", "--quiet"])
                    .arg(&url)
                    .arg(dir),
            )?;
            let _ = output;
        }

        let output = run_checked(
            self.runner.as_ref(),
            &CommandSpec::new("git")
                .args(["-C"])
                .arg(dir)
                .args(["rev-parse", "HEAD"]),
        )?;
        Ok(output.stdout.trim().to_string())
    }

    /// Resolve the current remote HEAD, used by `--devel` upgrades.
    pub fn remote_head(&self, package_base: &str) -> AurResult<String> {
        let output = run_checked(
            self.runner.as_ref(),
            &CommandSpec::new("git").args(["ls-remote", &self.url(package_base), "HEAD"]),
        )?;
        let head = output
            .stdout
            .split_whitespace()
            .next()
            .ok_or_else(|| AurError::Api(format!("empty ls-remote for {package_base}")))?;
        Ok(head.to_string())
    }

    pub fn read_srcinfo(&self, dir: &Path) -> AurResult<SrcInfo> {
        read_srcinfo(dir)
    }

    /// Build directory for a base below `root`.
    pub fn build_path(&self, root: &Path, package_base: &str) -> PathBuf {
        root.join(package_base)
    }

    fn run_git(&self, cwd: &Path, args: &[&str]) -> AurResult<()> {
        let output = run_checked(
            self.runner.as_ref(),
            &CommandSpec::new("git").arg("-C").arg(cwd).args(args),
        )?;
        let _ = output;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use xpm_aur_core::process::{CommandOutput, ProcessError};

    #[derive(Default)]
    struct RecordingRunner {
        calls: Mutex<Vec<CommandSpec>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, ProcessError> {
            self.calls.lock().unwrap().push(spec.clone());
            Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: "deadbeef\n".into(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn checkout_uses_package_base_url() {
        let runner = Arc::new(RecordingRunner::default());
        let git = AurGit::new("https://aur.archlinux.org", runner.clone());
        let dir = std::env::temp_dir().join(format!("xpm-aur-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let commit = git.checkout("demo", &dir).unwrap();
        assert_eq!(commit, "deadbeef");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].args[0], "clone");
        assert!(
            calls[0]
                .args
                .iter()
                .any(|arg| arg.to_string_lossy() == "https://aur.archlinux.org/demo.git")
        );
        assert_eq!(calls[1].args[0], "-C");
        assert_eq!(calls[1].args[2], "rev-parse");
    }
}
