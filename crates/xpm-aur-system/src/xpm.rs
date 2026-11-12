//! Adapter for all package transactions through xpm.
//!
//! xpm-aur never calls pacman to install or remove packages. It may use
//! `makepkg` to build AUR sources, but every resulting transaction is handed to
//! `xpm`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use xpm_aur_core::process::{CommandRunner, CommandSpec, ProcessError, StdioMode};

use crate::process::SystemRunner;

#[derive(Debug, Error)]
pub enum XpmError {
    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error("{command} failed with status {code:?}")]
    Failed { command: String, code: Option<i32> },
}

/// Thin, explicit wrapper around the xpm CLI.
pub struct XpmClient {
    runner: Arc<dyn CommandRunner>,
    program: String,
    sudo: Option<String>,
    no_confirm: bool,
}

impl XpmClient {
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        program: impl Into<String>,
        sudo: Option<String>,
        no_confirm: bool,
    ) -> Self {
        Self {
            runner,
            program: program.into(),
            sudo,
            no_confirm,
        }
    }

    /// Convenience constructor used by the CLI.
    pub fn system(no_confirm: bool) -> Self {
        Self::new(
            Arc::new(SystemRunner),
            "xpm",
            Some("sudo".into()),
            no_confirm,
        )
    }

    /// Clone this client with a different internal confirmation policy.
    ///
    /// xpm-aur confirms the whole plan once; internal transactions must never
    /// prompt a second time.
    pub fn with_no_confirm(&self, no_confirm: bool) -> Self {
        Self {
            runner: self.runner.clone(),
            program: self.program.clone(),
            sudo: self.sudo.clone(),
            no_confirm,
        }
    }

    /// Best-effort sudo credential pre-warm before parallel builds.
    ///
    /// Uses `sudo -n -v` so scripts without a TTY never block on a password
    /// prompt. The real transactions still ask for credentials if needed.
    pub fn authenticate(&self) -> Result<(), XpmError> {
        if is_effective_root() {
            return Ok(());
        }
        let Some(sudo) = &self.sudo else {
            return Ok(());
        };
        let spec = CommandSpec::new(sudo)
            .args(["-n", "-v"])
            .stdio(StdioMode::Capture);
        let _ = self.runner.run(&spec);
        Ok(())
    }

    pub fn sync(&self) -> Result<(), XpmError> {
        self.run(["sync"], &[], &[])?;
        Ok(())
    }

    pub fn upgrade(&self) -> Result<(), XpmError> {
        self.run(["upgrade"], &[], &[])?;
        Ok(())
    }

    pub fn install_official(
        &self,
        names: &[String],
        asdeps: bool,
        needed: bool,
    ) -> Result<(), XpmError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut options = Vec::new();
        if asdeps {
            options.push("--as-deps");
        }
        if needed {
            options.push("--needed");
        }
        self.run(["install"], &options, names)?;
        Ok(())
    }

    pub fn install_files(&self, paths: &[PathBuf], asdeps: bool) -> Result<(), XpmError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut options = Vec::new();
        if asdeps {
            options.push("--as-deps");
        }
        let names: Vec<OsString> = paths
            .iter()
            .map(|path| path.as_os_str().to_os_string())
            .collect();
        self.run_os(["install"], &options, &names)?;
        Ok(())
    }

    pub fn install_official_explicit(
        &self,
        names: &[String],
        needed: bool,
    ) -> Result<(), XpmError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut options = vec!["--as-explicit"];
        if needed {
            options.push("--needed");
        }
        self.run(["install"], &options, names)?;
        Ok(())
    }

    pub fn install_files_explicit(&self, paths: &[PathBuf]) -> Result<(), XpmError> {
        if paths.is_empty() {
            return Ok(());
        }
        let names: Vec<OsString> = paths
            .iter()
            .map(|path| path.as_os_str().to_os_string())
            .collect();
        self.run_os(["install"], &["--as-explicit"], &names)?;
        Ok(())
    }

    /// Remove filesystem trees (for example chroot roots) with `rm -rf`.
    pub fn remove_paths(&self, paths: &[PathBuf]) -> Result<(), XpmError> {
        self.remove_paths_with_root(paths, is_effective_root())
    }

    fn remove_paths_with_root(&self, paths: &[PathBuf], own_root: bool) -> Result<(), XpmError> {
        if paths.is_empty() {
            return Ok(());
        }
        let (program, args) = remove_paths_command(own_root, self.sudo.as_deref(), paths);
        self.execute(program, args)
    }

    pub fn remove(&self, names: &[String], recursive: bool, nosave: bool) -> Result<(), XpmError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut options = Vec::new();
        if recursive {
            options.push("--recursive");
        }
        if nosave {
            options.push("--nosave");
        }
        self.run(["remove"], &options, names)?;
        Ok(())
    }

    fn run(
        &self,
        command: impl IntoIterator<Item = &'static str>,
        options: &[&str],
        names: &[String],
    ) -> Result<(), XpmError> {
        let names: Vec<OsString> = names.iter().map(OsString::from).collect();
        self.run_os(command, options, &names)
    }

    fn run_os(
        &self,
        command: impl IntoIterator<Item = &'static str>,
        options: &[&str],
        names: &[OsString],
    ) -> Result<(), XpmError> {
        let own_root = is_effective_root();
        let mut args: Vec<OsString> = Vec::new();
        if self.no_confirm {
            args.push("--no-confirm".into());
        }
        args.extend(command.into_iter().map(OsString::from));
        args.extend(options.iter().map(OsString::from));
        if !names.is_empty() {
            args.push("--".into());
            args.extend(names.iter().cloned());
        }

        let (program, full_args) = if !own_root {
            if let Some(sudo) = &self.sudo {
                let mut full = Vec::with_capacity(args.len() + 1);
                full.push(OsString::from(&self.program));
                full.extend(args);
                (sudo.clone(), full)
            } else {
                (self.program.clone(), args)
            }
        } else {
            (self.program.clone(), args)
        };

        self.execute(program, full_args)
    }

    fn execute(&self, program: impl Into<OsString>, args: Vec<OsString>) -> Result<(), XpmError> {
        let spec = CommandSpec::new(program)
            .args(args.clone())
            .stdio(StdioMode::Inherit);
        let output = self.runner.run(&spec)?;
        if output.success {
            Ok(())
        } else {
            Err(XpmError::Failed {
                command: format!(
                    "{} {}",
                    spec.program.to_string_lossy(),
                    args.iter()
                        .map(|arg| arg.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                code: output.code,
            })
        }
    }
}

fn remove_paths_command(
    own_root: bool,
    sudo: Option<&str>,
    paths: &[PathBuf],
) -> (OsString, Vec<OsString>) {
    let mut args: Vec<OsString> = Vec::new();
    let program = if !own_root {
        match sudo {
            Some(sudo) => {
                args.push("rm".into());
                OsString::from(sudo)
            }
            None => OsString::from("rm"),
        }
    } else {
        OsString::from("rm")
    };
    args.push("-rf".into());
    args.push("--".into());
    args.extend(paths.iter().map(|path| path.as_os_str().to_os_string()));
    (program, args)
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
    use std::sync::Mutex;

    use std::path::Path;
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
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn sends_install_to_xpm_with_global_options_first() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .install_official(&["foo".into(), "bar".into()], true, true)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let args: Vec<String> = calls[0]
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--no-confirm",
                "install",
                "--as-deps",
                "--needed",
                "--",
                "foo",
                "bar"
            ]
        );
    }

    #[test]
    fn sends_local_files_to_xpm() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .install_files(&[Path::new("/tmp/demo.pkg.tar.zst").to_path_buf()], false)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert!(
            calls[0]
                .args
                .iter()
                .any(|arg| arg.to_string_lossy() == "/tmp/demo.pkg.tar.zst")
        );
    }

    fn args_of(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn installs_official_packages_as_explicit() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .install_official_explicit(&["foo".into(), "bar".into()], true)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].program, "xpm");
        assert_eq!(
            args_of(&calls[0]),
            vec![
                "--no-confirm",
                "install",
                "--as-explicit",
                "--needed",
                "--",
                "foo",
                "bar"
            ]
        );
    }

    #[test]
    fn installs_files_as_explicit() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .install_files_explicit(&[Path::new("/tmp/demo.pkg.tar.zst").to_path_buf()])
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].program, "xpm");
        assert_eq!(
            args_of(&calls[0]),
            vec![
                "--no-confirm",
                "install",
                "--as-explicit",
                "--",
                "/tmp/demo.pkg.tar.zst"
            ]
        );
    }

    #[test]
    fn removes_paths_through_sudo_when_not_root() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", Some("sudo".into()), true);
        client
            .remove_paths_with_root(
                &[
                    PathBuf::from("/tmp/build-root"),
                    PathBuf::from("/var/lib/archbuild/xpm-aur"),
                ],
                false,
            )
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].program, "sudo");
        assert_eq!(calls[0].stdio, StdioMode::Inherit);
        assert_eq!(
            args_of(&calls[0]),
            vec![
                "rm",
                "-rf",
                "--",
                "/tmp/build-root",
                "/var/lib/archbuild/xpm-aur"
            ]
        );
    }

    #[test]
    fn removes_paths_directly_when_root() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", Some("sudo".into()), true);
        client
            .remove_paths_with_root(&[PathBuf::from("/tmp/build-root")], true)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].program, "rm");
        assert_eq!(args_of(&calls[0]), vec!["-rf", "--", "/tmp/build-root"]);
    }

    #[test]
    fn remove_paths_without_sudo_runs_rm_directly() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .remove_paths_with_root(&[PathBuf::from("/tmp/build-root")], false)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].program, "rm");
        assert_eq!(args_of(&calls[0]), vec!["-rf", "--", "/tmp/build-root"]);
    }

    #[test]
    fn empty_remove_paths_does_not_run_a_command() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", Some("sudo".into()), true);
        client.remove_paths_with_root(&[], false).unwrap();
        assert!(runner.calls.lock().unwrap().is_empty());
    }
}
